//! Forms through the ACP client (packages/cli/src/acp/elicitation.ts): a credential-free form (the question tool's,
//! or a websearch provider's) becomes an `elicitation/create` request with a JSON schema of its fields for a client
//! that supports form elicitation; the answer becomes the form's reply, anything else cancels it. A form no client
//! can be shown is cancelled, a question with a message the model continues from.

use opencode_protocol::Json;

use super::translate::{Child, child_tool_call_id, prefix_title};
use crate::cli::client::Client;
use crate::json::{self, Obj, get, get_bool, get_str};

pub const UNSHOWN_QUESTION_MESSAGE: &str = "The question couldn't be shown to the user in this client. Continue without an answer: make reasonable assumptions and state them, or ask the user in your reply if you can't proceed.";

pub struct Ask {
    pub form: Json,
    pub schema: Json,
    pub client_session_id: String,
    pub child: Option<Child>,
    pub tool_call_sent: bool,
}

fn fields(form: &Json) -> &[Json] {
    get(form, "fields").and_then(json::array).unwrap_or(&[])
}

fn kind(field: &Json) -> &str {
    get_str(field, "type").unwrap_or("")
}

fn key(field: &Json) -> &str {
    get_str(field, "key").unwrap_or("")
}

/// `requestedSchema`: the form as an elicitation schema, if this client may be shown it.
pub fn requested_schema(form: &Json, form_elicitation: bool) -> Option<Json> {
    if !form_elicitation {
        return None;
    }
    if !matches!(get(form, "metadata").and_then(|m| get_str(m, "kind")), Some("question" | "websearch.provider")) {
        return None;
    }
    let fields = fields(form);
    if fields.iter().any(credential_like) {
        return None;
    }
    if fields.iter().any(|f| kind(f) == "external") || fields.iter().any(|f| get(f, "when").and_then(json::array).is_some_and(|w| !w.is_empty())) {
        return None;
    }
    if fields.iter().any(|f| get_bool(f, "hidden") == Some(true) && get_bool(f, "required") == Some(true) && get(f, "default").is_none()) {
        return None;
    }
    let keys: Vec<&str> = fields.iter().map(key).collect();
    let visible: Vec<&Json> = fields.iter().filter(|f| get_bool(f, "hidden") != Some(true)).collect();
    if !visible.iter().all(|f| representable(f, &keys)) {
        return None;
    }
    let mut properties = Obj::new();
    for field in &visible {
        for (name, schema) in field_properties(field) {
            properties.set(&name, schema);
        }
    }
    let required: Vec<Json> = visible.iter().filter(|f| get_bool(f, "required") == Some(true)).map(|f| Json::String(key(f).to_owned())).collect();
    let mut o = Obj::new();
    o.str("type", "object").set("properties", properties.done()).set("required", Json::Array(required));
    Some(o.done())
}

fn credential_like(field: &Json) -> bool {
    let mut texts: Vec<&str> = vec![key(field)];
    texts.extend(get_str(field, "title"));
    texts.extend(get_str(field, "description"));
    if matches!(kind(field), "string" | "multiselect") {
        texts.extend(options(field).iter().filter_map(|o| get_str(o, "label")));
    }
    texts.iter().any(|text| credential(text))
}

/// `/password|passphrase|secret|token|api[_ -]?key|credential|private[_ -]?key/i`.
fn credential(text: &str) -> bool {
    let lower = text.to_lowercase();
    if ["password", "passphrase", "secret", "token", "credential"].iter().any(|w| lower.contains(w)) {
        return true;
    }
    ["api", "private"].iter().any(|prefix| {
        lower.match_indices(prefix).any(|(at, _)| {
            let rest = &lower[at + prefix.len()..];
            let rest = rest.strip_prefix(['_', ' ', '-']).unwrap_or(rest);
            rest.starts_with("key")
        })
    })
}

fn options(field: &Json) -> &[Json] {
    get(field, "options").and_then(json::array).unwrap_or(&[])
}

fn has_options(field: &Json) -> bool {
    kind(field) == "multiselect" || get(field, "options").is_some()
}

fn custom_key(field: &Json) -> String {
    format!("{}_custom", key(field))
}

fn representable(field: &Json, keys: &[&str]) -> bool {
    if !matches!(kind(field), "string" | "multiselect") || !has_options(field) {
        return true;
    }
    let values: Vec<&str> = options(field).iter().filter_map(|o| get_str(o, "value")).collect();
    let defaults: Vec<&str> = match get(field, "default") {
        None => vec![],
        Some(Json::String(value)) => vec![value.as_str()],
        Some(Json::Array(items)) => items.iter().filter_map(json::as_str).collect(),
        Some(_) => vec![],
    };
    if defaults.iter().any(|d| !values.contains(d)) {
        return false;
    }
    if get_bool(field, "custom") != Some(true) {
        return true;
    }
    if get_bool(field, "required") == Some(true) || keys.contains(&custom_key(field).as_str()) {
        return false;
    }
    kind(field) == "string" || (get(field, "minItems").is_none() && get(field, "maxItems").is_none())
}

fn field_properties(field: &Json) -> Vec<(String, Json)> {
    let base = |o: &mut Obj| {
        o.opt("title", get(field, "title").cloned()).opt("description", get(field, "description").cloned());
    };
    let required = get_bool(field, "required") == Some(true);
    let custom = get_bool(field, "custom") == Some(true);
    let name = key(field).to_owned();
    match kind(field) {
        "string" => {
            let mut o = Obj::new();
            o.str("type", "string");
            base(&mut o);
            if !has_options(field) {
                string_constraints(field, &mut o);
                o.opt("default", get(field, "default").cloned());
                return vec![(name, o.done())];
            }
            o.set("oneOf", enum_options(field)).opt("default", get(field, "default").cloned());
            let mut out = vec![(name, o.done())];
            if custom {
                out.push(other(field, "Type your own answer"));
            }
            out
        }
        "multiselect" => {
            let mut items = Obj::new();
            items.set("anyOf", enum_options(field));
            let mut o = Obj::new();
            o.str("type", "array");
            base(&mut o);
            o.set("items", items.done());
            let min = get(field, "minItems").and_then(|v| if let Json::Number(n) = v { Some(*n) } else { None });
            if required {
                o.set("minItems", Json::Number(min.unwrap_or(0.0).max(1.0)));
            } else {
                o.opt("minItems", get(field, "minItems").cloned());
            }
            o.opt("maxItems", get(field, "maxItems").cloned()).opt("default", get(field, "default").cloned());
            let mut out = vec![(name, o.done())];
            if custom {
                out.push(other(field, "Add your own answer"));
            }
            out
        }
        "number" | "integer" => {
            let mut o = Obj::new();
            o.str("type", kind(field));
            base(&mut o);
            o.opt("minimum", get(field, "minimum").cloned()).opt("maximum", get(field, "maximum").cloned()).opt("default", get(field, "default").cloned());
            vec![(name, o.done())]
        }
        _ => {
            let mut o = Obj::new();
            o.str("type", "boolean");
            base(&mut o);
            o.opt("default", get(field, "default").cloned());
            vec![(name, o.done())]
        }
    }
}

fn other(field: &Json, description: &str) -> (String, Json) {
    let mut o = Obj::new();
    o.str("type", "string").str("title", &format!("{} (other)", get_str(field, "title").unwrap_or(key(field)))).str("description", description);
    if kind(field) == "string" {
        string_constraints(field, &mut o);
    }
    (custom_key(field), o.done())
}

/// Core rejects an empty string for a required field.
fn string_constraints(field: &Json, o: &mut Obj) {
    o.opt("format", get(field, "format").cloned());
    if get_bool(field, "required") == Some(true) {
        let min = match get(field, "minLength") {
            Some(Json::Number(n)) => *n,
            _ => 0.0,
        };
        o.set("minLength", Json::Number(min.max(1.0)));
    } else {
        o.opt("minLength", get(field, "minLength").cloned());
    }
    o.opt("maxLength", get(field, "maxLength").cloned()).opt("pattern", get(field, "pattern").cloned());
}

fn enum_options(field: &Json) -> Json {
    Json::Array(
        options(field)
            .iter()
            .map(|option| {
                let mut o = Obj::new();
                o.opt("const", get(option, "value").cloned()).opt("title", get(option, "label").cloned()).opt("description", get(option, "description").cloned());
                o.done()
            })
            .collect(),
    )
}

/// The `elicitation/create` parameters (`ACPElicitation.ask`).
pub fn request(ask: &Ask) -> Json {
    let source = if ask.tool_call_sent { get(&ask.form, "metadata").and_then(|m| get(m, "tool")).and_then(|t| get_str(t, "id")) } else { None };
    let mut o = Obj::new();
    o.str("mode", "form").str("sessionId", &ask.client_session_id);
    if let Some(id) = source {
        o.str("toolCallId", &child_tool_call_id(ask.child.as_ref(), id));
    }
    o.str("message", &prefix_title(ask.child.as_ref(), get_str(&ask.form, "title").unwrap_or(""))).set("requestedSchema", ask.schema.clone());
    o.done()
}

/// The form's answer from the client's response, or `None` to cancel it.
pub fn answer(form: &Json, response: &std::result::Result<Json, Json>) -> Option<Json> {
    let response = response.as_ref().ok()?;
    if get_str(response, "action") != Some("accept") {
        return None;
    }
    let content = match get(response, "content") {
        None | Some(Json::Null) => json::object(),
        Some(content) => content.clone(),
    };
    let Json::Object(entries) = &content else { return None };
    let valid = |value: &Json| match value {
        Json::String(_) | Json::Number(_) | Json::Bool(_) => true,
        Json::Array(items) => items.iter().all(|i| matches!(i, Json::String(_))),
        _ => false,
    };
    if !entries.values().all(valid) {
        return None;
    }
    let mut out = Obj::new();
    for field in fields(form) {
        if let Some(value) = field_answer(field, &content) {
            out.set(key(field), value);
        }
    }
    Some(out.done())
}

fn field_answer(field: &Json, content: &Json) -> Option<Json> {
    if kind(field) != "external" && get_bool(field, "hidden") == Some(true) {
        return get(field, "default").cloned();
    }
    let value = get(content, key(field)).cloned();
    if !matches!(kind(field), "string" | "multiselect") || get_bool(field, "custom") != Some(true) || !has_options(field) {
        return value;
    }
    let Some(custom) = get_str(content, &custom_key(field)).filter(|c| !c.trim_matches(crate::text::is_js_whitespace).is_empty()) else { return value };
    if kind(field) == "string" {
        return Some(Json::String(custom.to_owned()));
    }
    Some(match value {
        Some(Json::Array(mut items)) => {
            items.push(Json::String(custom.to_owned()));
            Json::Array(items)
        }
        _ => Json::Array(vec![Json::String(custom.to_owned())]),
    })
}

/// `cancel`: the form cancelled (with a message for the model); if that fails, the session is interrupted.
pub async fn cancel(client: &Client, form: &Json, message: Option<&str>) {
    let session = get_str(form, "sessionID").unwrap_or("").to_owned();
    let id = get_str(form, "id").unwrap_or("").to_owned();
    let mut query = Obj::new();
    query.opt_str("message", message);
    match client.call("session.form.cancel", &[("sessionID", &session), ("formID", &id)], Some(query.done()), None).await {
        Ok(_) => {}
        Err(error) if matches!(error.tag(), Some("FormAlreadySettledError" | "FormNotFoundError")) => {}
        Err(_) => {
            if session.starts_with("ses") {
                let _ = client.call("session.interrupt", &[("sessionID", &session)], None, None).await;
            }
        }
    }
}

/// `cancelUnshown`: a question the client cannot show is cancelled with a message, so the turn continues.
pub async fn cancel_unshown(client: &Client, form: &Json) {
    let question = get(form, "metadata").and_then(|m| get_str(m, "kind")) == Some("question");
    cancel(client, form, question.then_some(UNSHOWN_QUESTION_MESSAGE)).await
}

/// `ACPElicitation.respond`.
pub async fn respond(client: &Client, form: &Json, outcome: Option<Json>) {
    let Some(answer) = outcome else { return cancel(client, form, None).await };
    let session = get_str(form, "sessionID").unwrap_or("").to_owned();
    let id = get_str(form, "id").unwrap_or("").to_owned();
    let mut body = Obj::new();
    body.set("answer", answer);
    match client.call("session.form.reply", &[("sessionID", &session), ("formID", &id)], None, Some(body.done())).await {
        Ok(_) => {}
        Err(error) if matches!(error.tag(), Some("FormAlreadySettledError" | "FormNotFoundError")) => {}
        Err(_) => cancel(client, form, None).await,
    }
}
