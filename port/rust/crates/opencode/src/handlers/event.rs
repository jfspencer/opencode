//! The event stream (handlers/event.ts).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use opencode_protocol::Json;
use tokio::sync::broadcast::error::RecvError;

use crate::api::{Decoded, event_stream};
use crate::app::App;
use crate::http::{Request, Response, Result};
use crate::json;
use crate::location::Location;

pub async fn handle(app: &App, request: &Request, decoded: Decoded, _located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "event.subscribe" => Ok(subscribe(app, &decoded)),
        _ => super::fallback(app, request),
    }
}

/// `GET /api/event`: a `server.connected` event, then every public event as it is published, with a comment
/// heartbeat every 15 seconds.
fn subscribe(app: &App, decoded: &Decoded) -> Response {
    let types: Option<Vec<String>> = match json::get(&decoded.query, "types") {
        Some(Json::Array(items)) => Some(
            items
                .iter()
                .filter_map(|i| match i {
                    Json::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
        ),
        Some(Json::String(s)) => Some(vec![s.clone()]),
        _ => None,
    }
    .filter(|t| !t.is_empty());
    let mut receiver = app.bus.subscribe();
    let (sender, response) = event_stream();
    let connected = format!("data: {{\"id\":\"{}\",\"type\":\"server.connected\",\"data\":{{}}}}\n\n", crate::id::event());
    tokio::spawn(async move {
        if sender.send(Bytes::from(connected)).await.is_err() {
            return;
        }
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        loop {
            tokio::select! {
                event = receiver.recv() => match event {
                    Ok(event) => {
                        if types.as_ref().is_some_and(|types| !types.iter().any(|t| t == event.ty)) {
                            continue;
                        }
                        let frame = format!("data: {}\n\n", event.to_json_text());
                        if sender.send(Bytes::from(frame)).await.is_err() {
                            return;
                        }
                    }
                    // A subscriber that falls too far behind is dropped (EventFeed.SubscriberOverflow).
                    Err(RecvError::Lagged(_)) | Err(RecvError::Closed) => return,
                },
                _ = heartbeat.tick() => {
                    if sender.send(Bytes::from_static(b": heartbeat\n\n")).await.is_err() {
                        return;
                    }
                }
                _ = sender.closed() => return,
            }
        }
    });
    response
}
