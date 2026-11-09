//! Everything without an area of its own: generation, voice, media, artifacts, rpc (handlers/*.ts).

use std::sync::Arc;

use crate::api::Decoded;
use crate::app::App;
use crate::http::{Request, Response, Result};
use crate::location::Location;

pub async fn handle(app: &App, request: &Request, decoded: Decoded, _located: Option<Arc<Location>>) -> Result<Response> {
    match request.route.operation {
        "experimental.generate.text" | "experimental.generate.textLegacy" => {
            crate::generate::text(app, &decoded.payload).await.map(super::data)
        }
        _ => super::fallback(app, request),
    }
}
