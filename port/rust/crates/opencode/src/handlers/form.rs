//! Forms (handlers/form.ts, and the session form endpoints of handlers/session.ts): the location's pending forms,
//! and a session's forms created, read, answered and cancelled. The form middleware has loaded the location (the
//! `global` session's is the request's).

use std::sync::Arc;

use opencode_protocol::Json;

use super::{encoded, located};
use crate::api::Decoded;
use crate::app::App;
use crate::form::{CreateError, CreateInput, NotFound, SettleError};
use crate::http::{Failure, Request, Response, Result};
use crate::json::{self, Obj};
use crate::location::Location;

pub async fn handle(app: &App, request: &Request, decoded: Decoded, located_in: Option<Arc<Location>>) -> Result<Response> {
    let Some(location) = located_in else { return super::fallback(app, request) };
    let forms = &location.forms;
    let bus = &app.bus;
    let reference = location.reference();
    let session_id = request.param("sessionID");
    match request.route.operation {
        "form.list" => Ok(located(request, &location, Json::Array(forms.list(None)))),
        "session.form.list" => Ok(encoded(request, &data_of(Json::Array(forms.list(Some(session_id)))))),
        "session.form.create" => {
            let payload = &decoded.payload;
            let input = CreateInput {
                id: json::get_str(payload, "id").map(str::to_owned),
                session_id: session_id.to_owned(),
                title: json::get_str(payload, "title").unwrap_or_default().to_owned(),
                metadata: json::get(payload, "metadata").cloned(),
                fields: json::get(payload, "fields").cloned().unwrap_or(Json::Array(vec![])),
            };
            match forms.create(bus, &reference, input) {
                Ok(form) => Ok(encoded(request, &data_of(form))),
                Err(error) => {
                    let message = error.message();
                    match error {
                        CreateError::AlreadyExists(id) => {
                            let mut o = Obj::new();
                            o.str("_tag", "ConflictError").str("message", &message).str("resource", &id);
                            Err(Failure::new(409, o.done()))
                        }
                        CreateError::Invalid(_) => Err(Failure::invalid(message, Some("fields"), None)),
                    }
                }
            }
        }
        "session.form.get" => {
            let form_id = request.param("formID");
            let mut detail = owned(&location, session_id, form_id)?;
            let state = forms.state(form_id).map_err(|NotFound(id)| missing(&id))?;
            json::set(&mut detail, "state", state.to_json());
            Ok(encoded(request, &data_of(detail)))
        }
        "session.form.reply" => {
            let form_id = request.param("formID");
            owned(&location, session_id, form_id)?;
            let answer = json::get(&decoded.payload, "answer").cloned().unwrap_or_else(json::object);
            forms.reply(bus, &reference, form_id, answer).map_err(settle_failure)?;
            Ok(Response::no_content())
        }
        "session.form.cancel" => {
            let form_id = request.param("formID");
            owned(&location, session_id, form_id)?;
            let message = json::get_str(&decoded.query, "message").map(str::to_owned);
            forms.cancel(bus, &reference, form_id, message).map_err(settle_failure)?;
            Ok(Response::no_content())
        }
        _ => super::fallback(app, request),
    }
}

fn data_of(value: Json) -> Json {
    let mut o = Obj::new();
    o.set("data", value);
    o.done()
}

/// `missingForm`: `FormNotFoundError`.
fn missing(id: &str) -> Failure {
    let mut o = Obj::new();
    o.str("_tag", "FormNotFoundError").str("id", id).str("message", &format!("Form not found: {id}"));
    Failure::new(404, o.done())
}

/// `requireOwnedForm`: the form, when the session owns it.
fn owned(location: &Location, session_id: &str, form_id: &str) -> Result<Json> {
    let form = location.forms.get(form_id).map_err(|NotFound(id)| missing(&id))?;
    if json::get_str(&form, "sessionID") != Some(session_id) {
        return Err(missing(form_id));
    }
    Ok(form)
}

fn settle_failure(error: SettleError) -> Failure {
    let tagged = |status: u16, tag: &str, id: &str, message: String| {
        let mut o = Obj::new();
        o.str("_tag", tag).str("id", id).str("message", &message);
        Failure::new(status, o.done())
    };
    match error {
        SettleError::NotFound(id) => missing(&id),
        SettleError::AlreadySettled(id) => tagged(409, "FormAlreadySettledError", &id, format!("Form already settled: {id}")),
        SettleError::InvalidAnswer(id, message) => tagged(400, "FormInvalidAnswerError", &id, message),
    }
}
