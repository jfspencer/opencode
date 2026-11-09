//! `opencode auth` (packages/cli/src/commands/handlers/auth/*.ts, ui/prompt.ts, ui/integration-picker.ts): integrations
//! and credentials through the server, with @clack/prompts' output and prompts ([`super::prompt`]). A login uses one of
//! the integration's methods: an API key, a reference to credentials managed elsewhere (`external`), a command the
//! server runs (a well-known provider's, discovered from its URL first) or OAuth in the browser; the form a method
//! asks for is answered with `--answer key=value` or on the terminal. Without an integration, method or credential
//! named, the pickers ask on the terminal.

use opencode_protocol::Json;

use super::client::Client;
use super::connection::{self, Args, Resolved};
use super::out::{Fail, Result, pretty, stderr, stdout};
use super::parse::{Globals, Input};
use super::prompt::{self, Choice, Clack};
use crate::form::formats;
use crate::json::{self, Obj, get, get_str};

/// `handlePromptErrors`: a cancelled prompt ends as cancelled (exit 130), a failure is logged in clack's frame (exit 1).
fn prompt_errors(clack: &Clack, result: Result) -> Result {
    match result {
        Err(fail) if fail.name == "Cancelled" => {
            clack.cancel("Cancelled");
            Ok(130)
        }
        Err(fail) => {
            clack.error(&fail.message);
            clack.outro("Failed");
            Ok(1)
        }
        ok => ok,
    }
}

/// The plain commands report failures as their bare message, exit 1.
fn plain(result: Result) -> Result {
    match result {
        Err(fail) => {
            stderr(&format!("{}\n", fail.message));
            Ok(1)
        }
        ok => ok,
    }
}

fn location() -> Json {
    let mut inner = Obj::new();
    inner.str("directory", &super::startup_directory());
    let mut o = Obj::new();
    o.set("location", inner.done());
    o.done()
}

fn interactive() -> bool {
    super::stdin_tty() && super::stdout_tty()
}

fn require_interactive(message: &str) -> Result<()> {
    if interactive() { Ok(()) } else { Err(Fail::error(message)) }
}

async fn load_integrations(client: &Client) -> Result<Vec<Json>> {
    // The model endpoint is the public readiness boundary for the initial plugin generation.
    client.call("model.default", &[], Some(location()), None).await?;
    let response = client.call("integration.list", &[], Some(location()), None).await?;
    let mut items = get(&response, "data").and_then(json::array).map(|a| a.to_vec()).unwrap_or_default();
    items.sort_by_key(|i| get_str(i, "id") != Some("opencode-go"));
    Ok(items)
}

fn resolve_integration<'a>(integrations: &'a [Json], target: &str) -> Result<&'a Json> {
    let normalized = target.trim_end_matches('/');
    if let Some(found) = integrations.iter().find(|i| get_str(i, "id") == Some(normalized)) {
        return Ok(found);
    }
    let matches: Vec<&Json> = integrations
        .iter()
        .filter(|i| get_str(i, "name").is_some_and(|n| n.to_lowercase() == normalized.to_lowercase()))
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(Fail::error(format!("Integration not found: {target}"))),
        _ => Err(Fail::error(format!(
            "Integration name \"{target}\" is ambiguous: {}",
            matches.iter().filter_map(|i| get_str(i, "id")).collect::<Vec<_>>().join(", ")
        ))),
    }
}

fn connections(integration: &Json) -> &[Json] {
    get(integration, "connections").and_then(json::array).unwrap_or(&[])
}

async fn server(globals: &Globals) -> Result<Resolved> {
    connection::resolve(globals, Args::default()).await
}

/// Runs `body` with a client, then ends a private server.
async fn with_client<F, Fut, T>(globals: &Globals, body: F) -> Result<T>
where
    F: FnOnce(Client) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let resolved = server(globals).await?;
    let result = body(resolved.client()).await;
    resolved.close().await;
    result
}

pub async fn command(name: &str, globals: &Globals, input: &Input) -> Result {
    match name {
        "list" => plain(list(globals, input).await),
        "export" => plain(export(globals, input).await),
        "import" => plain(import(globals, input).await),
        "login" => {
            let clack = Clack::new();
            let result = login(&clack, globals, input).await;
            prompt_errors(&clack, result)
        }
        "logout" | "switch" => {
            let clack = Clack::new();
            let result = account(&clack, globals, input, name == "logout").await;
            prompt_errors(&clack, result)
        }
        _ => Ok(0),
    }
}

async fn list(globals: &Globals, input: &Input) -> Result {
    let format_json = input.str("format") == Some("json");
    with_client(globals, |client| async move {
        let integrations: Vec<Json> =
            load_integrations(&client).await?.into_iter().filter(|i| !connections(i).is_empty()).collect();
        if format_json {
            let items = integrations
                .iter()
                .map(|i| {
                    let mut o = Obj::new();
                    o.opt("id", get(i, "id").cloned()).opt("name", get(i, "name").cloned()).opt("connections", get(i, "connections").cloned());
                    o.done()
                })
                .collect();
            stdout(&format!("{}\n", pretty(&Json::Array(items))));
            return Ok(0);
        }
        let rows: Vec<(String, String, &str)> = integrations
            .iter()
            .flat_map(|i| {
                let name = get_str(i, "name").unwrap_or("").to_owned();
                connections(i).iter().map(move |c| {
                    let stored = get_str(c, "type") == Some("credential");
                    let source = get_str(c, if stored { "label" } else { "name" }).unwrap_or("").to_owned();
                    (name.clone(), source, if stored { "stored" } else { "environment" })
                })
            })
            .collect();
        if rows.is_empty() {
            stdout("No authenticated integrations\n");
            return Ok(0);
        }
        let width = rows.iter().map(|r| r.0.encode_utf16().count()).max().unwrap_or(0) + 2;
        let pad = |text: &str, width: usize| format!("{text}{}", " ".repeat(width.saturating_sub(text.encode_utf16().count())));
        let text: Vec<String> = rows.iter().map(|(i, s, t)| format!("{}{}{t}", pad(i, width), pad(s, 28))).collect();
        stdout(&format!("{}\n", text.join("\n")));
        Ok(0)
    })
    .await
}

async fn export(globals: &Globals, input: &Input) -> Result {
    let target = input.str("target").map(String::from);
    with_client(globals, |client| async move {
        let integration = match &target {
            Some(target) => {
                let integrations = load_integrations(&client).await?;
                Some(get_str(resolve_integration(&integrations, target)?, "id").unwrap_or("").to_owned())
            }
            None => None,
        };
        let response = client.call("credential.list", &[], None, None).await?;
        let credentials: Vec<Json> = get(&response, "data")
            .and_then(json::array)
            .unwrap_or(&[])
            .iter()
            .filter(|c| integration.as_deref().is_none_or(|id| get_str(c, "integrationID") == Some(id)))
            .cloned()
            .collect();
        if super::stdout_tty() {
            stderr("Warning: the output contains secrets; redirect it to a file or pipe it to auth import\n");
        }
        stdout(&format!("{}\n", pretty(&Json::Array(credentials))));
        Ok(0)
    })
    .await
}

/// `Schema.fromJsonString(Schema.Array(CredentialLegacy.Entry))`, with its first issue's message.
fn decode_entries(text: &str) -> Result<Vec<Json>> {
    let schema = |message: &str| Fail::named("SchemaError", message);
    let value = json::parse(text).ok_or_else(|| schema("Expected a valid JSON string"))?;
    let Json::Array(items) = value else { return Err(schema("Expected array")) };
    for item in &items {
        let Json::Object(entry) = item else { return Err(schema("Expected object")) };
        for (key, kind) in [("id", "string"), ("integrationID", "string"), ("label", "string"), ("active", "boolean"), ("value", "object")] {
            let ok = match entry.get(key) {
                None => return Err(schema("Missing key")),
                Some(Json::String(_)) => kind == "string",
                Some(Json::Bool(_)) => kind == "boolean",
                Some(Json::Object(_)) => kind == "object",
                Some(_) => false,
            };
            if !ok {
                return Err(schema(&format!("Expected {kind}")));
            }
        }
    }
    Ok(items)
}

async fn import(globals: &Globals, input: &Input) -> Result {
    let file = input.str("file").map(String::from);
    if file.is_none() && super::stdin_tty() {
        return Err(Fail::error("Pipe auth export output into stdin or pass a file to import"));
    }
    let text = match &file {
        Some(file) => match std::fs::read(file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Fail::error(format!("ENOENT: no such file or directory, open '{file}'")));
            }
            Err(error) => return Err(Fail::error(error.to_string())),
        },
        None => {
            use std::io::Read;
            let mut text = String::new();
            let _ = std::io::stdin().read_to_string(&mut text);
            text
        }
    };
    let credentials = decode_entries(&text)?;
    with_client(globals, |client| async move {
        let existing = client.call("credential.list", &[], None, None).await?;
        let existing = get(&existing, "data").and_then(json::array).map(|a| a.to_vec()).unwrap_or_default();
        let ids: Vec<&str> = existing.iter().filter_map(|c| get_str(c, "id")).collect();
        let integrations: Vec<&str> = existing.iter().filter_map(|c| get_str(c, "integrationID")).collect();
        let mut imported = 0;
        for credential in &credentials {
            if ids.contains(&get_str(credential, "id").unwrap_or("")) {
                continue;
            }
            let integration = get_str(credential, "integrationID").unwrap_or("");
            let mut body = Obj::new();
            body.opt("id", get(credential, "id").cloned())
                .str("integrationID", integration)
                .opt("label", get(credential, "label").cloned())
                .opt("value", get(credential, "value").cloned())
                .set(
                    "activate",
                    Json::Bool(json::get_bool(credential, "active") == Some(true) && !integrations.contains(&integration)),
                );
            match client.call("credential.create", &[], None, Some(body.done())).await {
                Ok(_) => imported += 1,
                Err(error) if error.tag() == Some("ConflictError") => {}
                Err(error) => return Err(error.into()),
            }
        }
        let skipped = credentials.len() - imported;
        let mut line = format!("Imported {imported} {}", if imported == 1 { "credential" } else { "credentials" });
        if skipped > 0 {
            line.push_str(&format!(", skipped {skipped} already present"));
        }
        stderr(&format!("{line}\n"));
        Ok(0)
    })
    .await
}

/// `connectMethods`: every method but `env`, the key method last.
fn connect_methods(integration: &Json) -> Vec<Json> {
    let mut methods: Vec<Json> = get(integration, "methods")
        .and_then(json::array)
        .unwrap_or(&[])
        .iter()
        .filter(|m| get_str(m, "type") != Some("env"))
        .cloned()
        .collect();
    methods.sort_by_key(|m| get_str(m, "type") == Some("key"));
    methods
}

fn resolve_method(methods: &[Json], target: &str) -> Result<Json> {
    let normalized = target.to_lowercase();
    let matches: Vec<&Json> = methods
        .iter()
        .filter(|m| {
            let label = get_str(m, "label").map(|l| l.to_lowercase());
            if get_str(m, "type") == Some("key") {
                return normalized == "key" || label.as_deref() == Some(normalized.as_str());
            }
            get_str(m, "id") == Some(target) || label.as_deref() == Some(normalized.as_str())
        })
        .collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => {
            let available: Vec<&str> = methods
                .iter()
                .map(|m| if get_str(m, "type") == Some("key") { "key" } else { get_str(m, "id").unwrap_or("") })
                .collect();
            let available = available.join(", ");
            Err(Fail::error(format!(
                "Authentication method not found: {target}{}",
                if available.is_empty() { String::new() } else { format!(". Available: {available}") }
            )))
        }
        _ => Err(Fail::error(format!("Authentication method \"{target}\" is ambiguous"))),
    }
}

// --- forms (auth/form.ts) ---

/// What a choice of a form field's prompt stands for.
#[derive(Clone)]
enum Pick {
    Value(Json),
    /// "Type your own answer" / "Type another value".
    Custom,
    Skip,
}

/// `answerForm`: each active field answered from `--answer key=value`, its default when hidden, or on the terminal;
/// every answer validated as the form requires.
async fn answer_form(c: &Clack, fields: Option<&Json>, supplied: &[String]) -> Result<Option<Json>> {
    let fields = fields.and_then(json::array);
    let mut given: Vec<(String, String)> = vec![];
    for item in supplied {
        let Some(separator) = item.find('=').filter(|i| *i >= 1) else {
            return Err(Fail::error("Expected --answer key=value"));
        };
        let key = &item[..separator];
        if !fields.is_some_and(|f| f.iter().any(|field| get_str(field, "key") == Some(key))) {
            return Err(Fail::error(format!("Unknown form field: {key}")));
        }
        given.retain(|(k, _)| k != key);
        given.push((key.to_owned(), item[separator + 1..].to_owned()));
    }
    let Some(fields) = fields else { return Ok(None) };
    let mut answer = json::object();
    for field in fields {
        let key = get_str(field, "key").unwrap_or("");
        let kind = get_str(field, "type").unwrap_or("");
        let provided = given.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        if !active(field, &answer) {
            if provided.is_some() {
                return Err(Fail::error(format!("Form field is not active: {key}")));
            }
            continue;
        }
        let value = match provided {
            Some(provided) => Some(supplied_answer(field, provided)?),
            None if kind != "external" && json::get_bool(field, "hidden") == Some(true) => get(field, "default").cloned(),
            None => answer_field(c, field).await?,
        };
        if kind != "external" {
            if let Some(invalid) = validate_value(field, value.as_ref()) {
                return Err(Fail::error(format!("{key}: {invalid}")));
            }
        }
        if let Some(value) = value {
            json::set(&mut answer, key, value);
        }
    }
    Ok(Some(answer))
}

/// A field asks only when its conditions on earlier answers hold (`when`).
fn active(field: &Json, answer: &Json) -> bool {
    if get_str(field, "type") == Some("external") {
        return true;
    }
    let Some(conditions) = get(field, "when").and_then(json::array) else { return true };
    conditions.iter().all(|condition| {
        let Some(value) = get_str(condition, "key").and_then(|key| get(answer, key)) else { return false };
        let expected = get(condition, "value").cloned().unwrap_or(Json::Null);
        let matches = match value {
            Json::Array(items) => items.iter().any(|item| json::as_str(item) == Some(js_string(&expected).as_str())),
            value => *value == expected,
        };
        if get_str(condition, "op") == Some("eq") { matches } else { !matches }
    })
}

/// `suppliedAnswer`: text as given for text fields, JSON for the others.
fn supplied_answer(field: &Json, input: &str) -> Result<Json> {
    let key = get_str(field, "key").unwrap_or("");
    match get_str(field, "type") {
        Some("external") => Err(Fail::error(format!("External form field requires interactive confirmation: {key}"))),
        Some("string") => Ok(Json::String(input.to_owned())),
        _ => match json::parse(input) {
            Some(value @ (Json::String(_) | Json::Number(_) | Json::Bool(_))) => Ok(value),
            Some(Json::Array(items)) if items.iter().all(|i| matches!(i, Json::String(_))) => Ok(Json::Array(items)),
            _ => Err(Fail::error(format!("Expected a JSON value for form field: {key}"))),
        },
    }
}

/// `answerField`: the field's prompt.
async fn answer_field(c: &Clack, field: &Json) -> Result<Option<Json>> {
    require_interactive("Authentication form input requires an interactive terminal")?;
    let key = get_str(field, "key").unwrap_or("");
    let message = get_str(field, "title").unwrap_or(key).to_owned();
    if let Some(description) = get_str(field, "description") {
        c.info(description);
    }
    let required = json::get_bool(field, "required") == Some(true);
    let default = get(field, "default");
    let options: Vec<(String, String, Option<String>)> = get(field, "options")
        .and_then(json::array)
        .unwrap_or(&[])
        .iter()
        .map(|o| {
            let value = get_str(o, "value").unwrap_or("").to_owned();
            (value, get_str(o, "label").unwrap_or("").to_owned(), get_str(o, "description").map(str::to_owned))
        })
        .collect();
    let custom = json::get_bool(field, "custom") == Some(true);
    match get_str(field, "type").unwrap_or("") {
        "external" => {
            let url = get_str(field, "url").unwrap_or("");
            c.info(url);
            prompt::open_url(url);
            let question = if message.is_empty() { "Continue after completing this step?" } else { &message };
            if !prompt::confirm(c, question, true).await? {
                let step = if message.is_empty() { "External step" } else { &message };
                return Err(Fail::error(format!("{step} is required")));
            }
            Ok(Some(Json::Bool(true)))
        }
        "boolean" => {
            if required {
                let initial = default.and_then(|d| if let Json::Bool(b) = d { Some(*b) } else { None }).unwrap_or(true);
                return Ok(Some(Json::Bool(prompt::confirm(c, &message, initial).await?)));
            }
            let picks = [Pick::Value(Json::Bool(true)), Pick::Value(Json::Bool(false)), Pick::Skip];
            let choices = [choice("Yes", None), choice("No", None), choice("Skip", None)];
            let initial = match default {
                Some(Json::Bool(true)) => 0,
                Some(Json::Bool(false)) => 1,
                _ => 2,
            };
            match &picks[prompt::select(c, &message, &choices, initial).await?] {
                Pick::Value(value) => Ok(Some(value.clone())),
                _ => Ok(None),
            }
        }
        "multiselect" => {
            let mut choices: Vec<Choice> = options.iter().map(|(_, label, hint)| choice(label, hint.as_deref())).collect();
            if custom {
                choices.push(choice("Type another value", None));
            }
            let defaults: Vec<&str> = default.and_then(json::array).unwrap_or(&[]).iter().filter_map(json::as_str).collect();
            let initial: Vec<usize> =
                options.iter().enumerate().filter(|(_, (value, _, _))| defaults.contains(&value.as_str())).map(|(i, _)| i).collect();
            let min = json::get_num(field, "minItems");
            let chosen = prompt::multiselect(c, &message, &choices, initial, required || min.unwrap_or(0.0) > 0.0).await?;
            let mut selected: Vec<String> = chosen.iter().filter(|i| **i < options.len()).map(|i| options[*i].0.clone()).collect();
            if chosen.contains(&options.len()) && custom {
                let required = |value: Option<&str>| value.filter(|v| !v.is_empty()).is_none().then(|| "Required".to_owned());
                selected.push(prompt::text(c, "Enter value", None, None, &required).await?);
            }
            if let Some(min) = min.filter(|min| (selected.len() as f64) < *min) {
                return Err(Fail::error(format!("Select at least {}", js_number(min))));
            }
            if let Some(max) = json::get_num(field, "maxItems").filter(|max| (selected.len() as f64) > *max) {
                return Err(Fail::error(format!("Select at most {}", js_number(max))));
            }
            Ok(Some(Json::Array(selected.into_iter().map(Json::String).collect())))
        }
        kind => {
            if kind == "string" && get(field, "options").is_some() {
                let mut picks: Vec<Pick> = options.iter().map(|(value, _, _)| Pick::Value(Json::String(value.clone()))).collect();
                let mut choices: Vec<Choice> = options.iter().map(|(_, label, hint)| choice(label, hint.as_deref())).collect();
                if custom {
                    picks.push(Pick::Custom);
                    choices.push(choice("Type your own answer", None));
                }
                if !required {
                    picks.push(Pick::Skip);
                    choices.push(choice("Skip", None));
                }
                let initial = picks.iter().position(|p| matches!(p, Pick::Value(v) if Some(v) == default)).unwrap_or(0);
                match &picks[prompt::select(c, &message, &choices, initial).await?] {
                    Pick::Skip => return Ok(None),
                    Pick::Value(value) => return Ok(Some(value.clone())),
                    Pick::Custom => {}
                }
            }
            let placeholder = if kind == "string" { get_str(field, "placeholder") } else { None };
            let initial = default.map(js_string);
            let validate = |value: Option<&str>| validate_text(field, value);
            let value = prompt::text(c, &message, placeholder, initial.as_deref(), &validate).await?;
            if value.is_empty() && !required {
                return Ok(None);
            }
            if kind == "string" {
                return Ok(Some(Json::String(value)));
            }
            Ok(Some(Json::Number(to_number(&value))))
        }
    }
}

fn choice(label: &str, hint: Option<&str>) -> Choice {
    Choice { label: label.to_owned(), hint: hint.map(str::to_owned) }
}

/// `validateText`: what the text prompt of a field refuses.
fn validate_text(field: &Json, value: Option<&str>) -> Option<String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return (json::get_bool(field, "required") == Some(true)).then(|| "Required".to_owned());
    };
    let kind = get_str(field, "type").unwrap_or("");
    if kind == "number" || kind == "integer" {
        let number = to_number(value);
        if !number.is_finite() {
            return Some("Expected a number".into());
        }
        if kind == "integer" && number.fract() != 0.0 {
            return Some("Expected an integer".into());
        }
        if let Some(minimum) = json::get_num(field, "minimum").filter(|m| number < *m) {
            return Some(format!("Must be at least {}", js_number(minimum)));
        }
        if let Some(maximum) = json::get_num(field, "maximum").filter(|m| number > *m) {
            return Some(format!("Must be at most {}", js_number(maximum)));
        }
        return None;
    }
    let length = value.encode_utf16().count() as f64;
    if let Some(min) = json::get_num(field, "minLength").filter(|m| length < *m) {
        return Some(format!("Must be at least {} characters", js_number(min)));
    }
    if let Some(max) = json::get_num(field, "maxLength").filter(|m| length > *m) {
        return Some(format!("Must be at most {} characters", js_number(max)));
    }
    if let Some(pattern) = get_str(field, "pattern") {
        if !formats::js_regex(pattern).is_some_and(|regex| formats::test(&regex, value)) {
            return Some("Invalid format".into());
        }
    }
    match get_str(field, "format") {
        Some("uri") if !formats::url_can_parse(value) => Some("Expected a URL".into()),
        Some("email") if !formats::email(value) => Some("Expected an email address".into()),
        Some("date") if !formats::date(value) => Some("Expected a date".into()),
        Some("date-time") if !formats::date_time(value) => Some("Expected a date and time".into()),
        _ => None,
    }
}

/// `formValidateValue` (packages/tui/src/util/form.ts): what an answer must be for its field.
fn validate_value(field: &Json, value: Option<&Json>) -> Option<String> {
    let required = json::get_bool(field, "required") == Some(true);
    let kind = get_str(field, "type").unwrap_or("");
    let Some(value) = value else { return required.then(|| "Answer required".to_owned()) };
    let empty = matches!(value, Json::String(s) if s.is_empty()) || matches!(value, Json::Array(items) if items.is_empty());
    if required && empty {
        return Some(if kind == "multiselect" { "Select at least one option" } else { "Answer required" }.to_owned());
    }
    let options: Vec<&str> =
        get(field, "options").and_then(json::array).unwrap_or(&[]).iter().filter_map(|o| get_str(o, "value")).collect();
    let custom = json::get_bool(field, "custom") == Some(true);
    match kind {
        "string" => {
            let Json::String(value) = value else { return Some("Expected text".into()) };
            let length = value.encode_utf16().count() as f64;
            if let Some(min) = json::get_num(field, "minLength").filter(|m| length < *m) {
                return Some(format!("Must be at least {} characters", js_number(min)));
            }
            if let Some(max) = json::get_num(field, "maxLength").filter(|m| length > *m) {
                return Some(format!("Must be at most {} characters", js_number(max)));
            }
            if let Some(pattern) = get_str(field, "pattern") {
                match formats::js_regex(pattern) {
                    None => return Some(format!("Invalid pattern: {pattern}")),
                    Some(regex) if !formats::test(&regex, value) => return Some(format!("Must match pattern: {pattern}")),
                    _ => {}
                }
            }
            match get_str(field, "format") {
                Some("email") if !formats::email(value) => return Some("Expected an email address".into()),
                Some("uri") if !formats::url_can_parse(value) => return Some("Expected a URL".into()),
                Some("date") if !formats::date(value) => return Some("Expected a date (YYYY-MM-DD)".into()),
                Some("date-time") if !formats::date_time(value) => return Some("Expected a date and time".into()),
                _ => {}
            }
            if get(field, "options").is_some() && !custom && !options.contains(&value.as_str()) {
                return Some("Select an available option".into());
            }
            None
        }
        "number" | "integer" => {
            let Json::Number(number) = value else { return Some("Expected a number".into()) };
            if !number.is_finite() {
                return Some("Expected a number".into());
            }
            if kind == "integer" && number.fract() != 0.0 {
                return Some("Expected an integer".into());
            }
            if let Some(minimum) = json::get_num(field, "minimum").filter(|m| number < m) {
                return Some(format!("Must be at least {}", js_number(minimum)));
            }
            if let Some(maximum) = json::get_num(field, "maximum").filter(|m| number > m) {
                return Some(format!("Must be at most {}", js_number(maximum)));
            }
            None
        }
        "boolean" => (!matches!(value, Json::Bool(_))).then(|| "Expected yes or no".to_owned()),
        _ => {
            let Json::Array(items) = value else { return Some("Expected selections".into()) };
            if let Some(min) = json::get_num(field, "minItems").filter(|m| (items.len() as f64) < *m) {
                return Some(format!("Select at least {}", js_number(min)));
            }
            if let Some(max) = json::get_num(field, "maxItems").filter(|m| (items.len() as f64) > *m) {
                return Some(format!("Select at most {}", js_number(max)));
            }
            let unknown = items.iter().any(|item| !json::as_str(item).is_some_and(|i| options.contains(&i)));
            (!custom && unknown).then(|| "Select only available options".to_owned())
        }
    }
}

/// JS `String(value)` of a form value.
fn js_string(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Number(number) => js_number(*number),
        Json::Bool(flag) => flag.to_string(),
        Json::Null => "null".into(),
        Json::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        Json::Object(_) => "[object Object]".into(),
    }
}

/// JS `String(number)` for the numbers forms carry.
fn js_number(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e21 { format!("{}", number as i64) } else { format!("{number}") }
}

/// JS `Number(text)`: decimal (with exponent), `0x`/`0o`/`0b` integers, `Infinity`, blank as zero; anything else NaN.
fn to_number(text: &str) -> f64 {
    let text = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if text.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(digits) = text.strip_prefix(prefix) {
            return u128::from_str_radix(digits, radix).map(|n| n as f64).unwrap_or(f64::NAN);
        }
    }
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if unsigned == "Infinity" {
        return if text.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let decimal = !unsigned.is_empty()
        && unsigned.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
        && unsigned.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.');
    if decimal { text.parse().unwrap_or(f64::NAN) } else { f64::NAN }
}

// --- login (auth/login.ts) ---

/// The integrations offered first, in this order.
const PRIORITY: [&str; 6] = ["opencode-go", "opencode", "openai", "github-copilot", "anthropic", "google"];

async fn login(c: &Clack, globals: &Globals, input: &Input) -> Result {
    let target = input.str("target").map(String::from);
    if target.is_none() {
        require_interactive("Pass an integration ID or name when running without an interactive terminal")?;
    }
    c.intro("Connect an integration");
    let resolved = server(globals).await?;
    let client = resolved.client();
    let result = async {
        let integration = find_integration(c, &client, target.as_deref()).await?;
        let name = get_str(&integration, "name").unwrap_or("").to_owned();
        let methods = connect_methods(&integration);
        if methods.is_empty() {
            return Err(Fail::error(format!("{name} has no interactive login methods")));
        }
        let method = choose_method(c, &methods, input.str("method")).await?;
        let form = if get_str(&method, "type") == Some("command") { None } else { get(&method, "form") };
        let answer = answer_form(c, form, input.list("answer")).await?;
        match get_str(&method, "type") {
            Some("key") => key_login(c, &client, &integration, &method, answer).await?,
            Some("command") => command_login(c, &client, &integration, &method).await?,
            Some("external") => external_login(c, &client, &integration, &method, answer).await?,
            _ => oauth_login(c, &client, &integration, &method, answer).await?,
        }
        c.outro("Done");
        Ok(0)
    }
    .await;
    resolved.close().await;
    result
}

/// The integration to log in to: the one named (a well-known provider's URL is discovered first), else the one
/// picked on the terminal.
async fn find_integration(c: &Clack, client: &Client, target: Option<&str>) -> Result<Json> {
    if let Some(target) = target {
        if crate::url::Url::parse(target).is_some_and(|url| url.scheme == "http" || url.scheme == "https") {
            c.start();
            let mut body = Obj::new();
            body.str("url", target);
            match client.call("experimental.integration.wellknown.add", &[], Some(location()), Some(body.done())).await {
                Ok(_) => c.stop("Authentication provider discovered", 0),
                Err(error) => {
                    c.stop("Discovery failed", 1);
                    return Err(error.into());
                }
            }
        }
    }
    let integrations = load_integrations(client).await?;
    if let Some(target) = target {
        return resolve_integration(&integrations, target).cloned();
    }
    let choices = login_choices(&integrations);
    if choices.is_empty() {
        return Err(Fail::error("No authentication integrations are available"));
    }
    let id = prompt::select_integration(c, &choices).await?;
    resolve_integration(&integrations, &id).cloned()
}

/// `loginChoices`: the integrations with a login method, MCP servers first, then the popular ones, then by name.
fn login_choices(integrations: &[Json]) -> Vec<prompt::IntegrationChoice> {
    let mcp = |i: &Json| get(i, "metadata").and_then(|m| get_str(m, "source")) == Some("mcp");
    let priority = |i: &Json| PRIORITY.iter().position(|id| get_str(i, "id") == Some(id)).unwrap_or(PRIORITY.len());
    let mut items: Vec<&Json> = integrations.iter().filter(|i| !connect_methods(i).is_empty()).collect();
    items.sort_by(|a, b| {
        mcp(b)
            .cmp(&mcp(a))
            .then_with(|| priority(a).cmp(&priority(b)))
            .then_with(|| crate::collate::compare(get_str(a, "name").unwrap_or(""), get_str(b, "name").unwrap_or("")))
            .then_with(|| crate::collate::compare(get_str(a, "id").unwrap_or(""), get_str(b, "id").unwrap_or("")))
    });
    items
        .into_iter()
        .map(|i| prompt::IntegrationChoice {
            value: get_str(i, "id").unwrap_or("").to_owned(),
            label: get_str(i, "name").unwrap_or("").to_owned(),
            category: if mcp(i) {
                "MCP"
            } else if priority(i) < PRIORITY.len() {
                "Popular"
            } else {
                "Services"
            },
            connected: !connections(i).is_empty(),
        })
        .collect()
}

async fn choose_method(c: &Clack, methods: &[Json], target: Option<&str>) -> Result<Json> {
    if let Some(target) = target {
        return resolve_method(methods, target);
    }
    if methods.len() == 1 {
        return Ok(methods[0].clone());
    }
    require_interactive("Pass --method when running without an interactive terminal")?;
    let choices: Vec<Choice> = methods
        .iter()
        .map(|m| {
            let fallback = if get_str(m, "type") == Some("key") { "API key" } else { "" };
            choice(get_str(m, "label").unwrap_or(fallback), None)
        })
        .collect();
    let index = prompt::select(c, "Select login method", &choices, 0).await?;
    let value = if get_str(&methods[index], "type") == Some("key") { "key" } else { get_str(&methods[index], "id").unwrap_or("") };
    resolve_method(methods, value)
}

async fn key_login(c: &Clack, client: &Client, integration: &Json, method: &Json, answer: Option<Json>) -> Result<()> {
    let name = get_str(integration, "name").unwrap_or("");
    let message = get_str(method, "label").map(String::from).unwrap_or_else(|| format!("Enter your {name} API key"));
    require_interactive("API key input requires an interactive terminal")?;
    let key = prompt::password(c, &message).await?;
    c.start();
    let mut body = Obj::new();
    body.str("key", &key).opt("answer", answer);
    let id = get_str(integration, "id").unwrap_or("");
    finish(c, name, client.call("integration.connect.key", &[("integrationID", id)], Some(location()), Some(body.done())).await)
}

async fn external_login(c: &Clack, client: &Client, integration: &Json, method: &Json, answer: Option<Json>) -> Result<()> {
    let name = get_str(integration, "name").unwrap_or("");
    c.start();
    let mut body = Obj::new();
    body.str("methodID", get_str(method, "id").unwrap_or("")).opt("answer", answer);
    let id = get_str(integration, "id").unwrap_or("");
    finish(c, name, client.call("integration.connect.external", &[("integrationID", id)], Some(location()), Some(body.done())).await)
}

/// The spinner stops on a saved credential, or on a failure (which the command reports).
fn finish(c: &Clack, name: &str, saved: std::result::Result<Json, super::client::ClientError>) -> Result<()> {
    match saved {
        Ok(_) => {
            c.stop(&format!("Connected to {name}"), 0);
            Ok(())
        }
        Err(error) => {
            c.stop("Authentication failed", 1);
            Err(error.into())
        }
    }
}

/// `oauthLogin`: starts the attempt, shows (and opens) its URL, then completes it with a pasted code or waits for the
/// browser. The attempt is cancelled when the login ends however it ends (a settled one is left alone).
async fn oauth_login(c: &Clack, client: &Client, integration: &Json, method: &Json, answer: Option<Json>) -> Result<()> {
    let id = get_str(integration, "id").unwrap_or("").to_owned();
    c.start();
    let mut body = Obj::new();
    body.str("methodID", get_str(method, "id").unwrap_or("")).opt("answer", answer);
    let started = match client.call("integration.oauth.connect", &[("integrationID", &id)], Some(location()), Some(body.done())).await {
        Ok(started) => started,
        Err(error) => {
            c.stop("Authentication failed", 1);
            return Err(error.into());
        }
    };
    let attempt = get(&started, "data").cloned().unwrap_or_default();
    let attempt_id = get_str(&attempt, "attemptID").unwrap_or("").to_owned();
    let result = authorize(c, client, integration, &attempt).await;
    let params = [("integrationID", id.as_str()), ("attemptID", attempt_id.as_str())];
    let cancel = client.call("integration.oauth.cancel", &params, Some(location()), None);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), cancel).await;
    result
}

async fn authorize(c: &Clack, client: &Client, integration: &Json, attempt: &Json) -> Result<()> {
    let id = get_str(integration, "id").unwrap_or("");
    let name = get_str(integration, "name").unwrap_or("");
    let attempt_id = get_str(attempt, "attemptID").unwrap_or("");
    let url = get_str(attempt, "url").unwrap_or("");
    c.stop("Authorization started", 0);
    c.info(get_str(attempt, "instructions").unwrap_or(""));
    c.info(url);
    if interactive() {
        prompt::open_url(url);
    }
    let params = [("integrationID", id), ("attemptID", attempt_id)];
    if get_str(attempt, "mode") == Some("code") {
        require_interactive("This login requires an interactive terminal to enter the authorization code")?;
        let required = |value: Option<&str>| value.filter(|v| !v.is_empty()).is_none().then(|| "Required".to_owned());
        let code = prompt::text(c, "Paste the authorization code", None, None, &required).await?;
        c.start();
        let mut body = Obj::new();
        body.str("code", &code);
        return finish(c, name, client.call("integration.oauth.complete", &params, Some(location()), Some(body.done())).await);
    }
    // Waits that can last minutes print plain lines, and Ctrl+C ends them (cancelling the attempt).
    c.step("Waiting for authorization...");
    let wait = poll(client, "integration.oauth.status", &params, |_| {});
    let status = if json::get_bool(attempt, "paste") == Some(true) && interactive() {
        tokio::select! {
            status = wait => status?,
            never = paste_redirect(c, client, &params) => never?,
        }
    } else {
        interruptible(wait).await??
    };
    match get_str(&status, "status") {
        Some("complete") => {
            c.success(&format!("Connected to {name}"));
            Ok(())
        }
        Some("failed") => Err(Fail::error(get_str(&status, "message").unwrap_or(""))),
        _ => Err(Fail::error("Authorization expired")),
    }
}

/// `pasteRedirect`: hands the URL the browser was redirected to to the attempt (which settles through its status);
/// a refused one is reported and asked for again. It never ends by itself.
async fn paste_redirect(c: &Clack, client: &Client, params: &[(&str, &str); 2]) -> Result<Json> {
    let required = |value: Option<&str>| value.filter(|v| !v.is_empty()).is_none().then(|| "Required".to_owned());
    loop {
        let code =
            prompt::text(c, "If the browser can't reach this machine, paste the URL it was redirected to", None, None, &required).await?;
        let mut body = Obj::new();
        body.str("code", &code);
        match client.call("integration.oauth.complete", params, Some(location()), Some(body.done())).await {
            Ok(_) => return std::future::pending().await,
            Err(error) => c.error(&error.message()),
        }
    }
}

/// `commandLogin`: runs the method's command on the server, printing each complete line of its stderr once.
async fn command_login(c: &Clack, client: &Client, integration: &Json, method: &Json) -> Result<()> {
    let id = get_str(integration, "id").unwrap_or("").to_owned();
    let name = get_str(integration, "name").unwrap_or("");
    c.start();
    let mut body = Obj::new();
    body.str("methodID", get_str(method, "id").unwrap_or(""));
    let started = match client.call("integration.command.connect", &[("integrationID", &id)], Some(location()), Some(body.done())).await {
        Ok(started) => started,
        Err(error) => {
            c.stop("Authentication failed", 1);
            return Err(error.into());
        }
    };
    let attempt_id = get(&started, "data").and_then(|d| get_str(d, "attemptID")).unwrap_or("").to_owned();
    let params = [("integrationID", id.as_str()), ("attemptID", attempt_id.as_str())];
    let result = async {
        c.stop("Authentication command started", 0);
        c.step("Waiting for authentication command...");
        let mut printed = 0usize;
        let status = interruptible(poll(client, "integration.command.status", &params, |message| {
            let end = message.rfind('\n').map_or(0, |i| i + 1);
            if end <= printed {
                return;
            }
            let output = message[printed..end].trim().to_owned();
            printed = end;
            if !output.is_empty() {
                c.plain(&output);
            }
        }))
        .await??;
        match get_str(&status, "status") {
            Some("complete") => {
                c.success(&format!("Connected to {name}"));
                Ok(())
            }
            Some("failed") => Err(Fail::error(get_str(&status, "message").unwrap_or(""))),
            _ => Err(Fail::error("Authentication expired")),
        }
    }
    .await;
    let cancel = client.call("integration.command.cancel", &params, Some(location()), None);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), cancel).await;
    result
}

/// Polls an attempt's status every half second until it settles; a pending status's message goes to `update`.
async fn poll(client: &Client, operation: &str, params: &[(&str, &str)], mut update: impl FnMut(&str)) -> Result<Json> {
    loop {
        let response = client.call(operation, params, Some(location()), None).await?;
        let status = get(&response, "data").cloned().unwrap_or_default();
        if get_str(&status, "status") != Some("pending") {
            return Ok(status);
        }
        if let Some(message) = get_str(&status, "message").filter(|m| !m.is_empty()) {
            update(message);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// A wait that Ctrl+C interrupts (the command ends as cancelled).
async fn interruptible<T>(wait: impl std::future::Future<Output = T>) -> Result<T> {
    tokio::select! {
        value = wait => Ok(value),
        _ = tokio::signal::ctrl_c() => Err(prompt::cancelled()),
    }
}

// --- logout and switch (auth/account.ts) ---

async fn account(c: &Clack, globals: &Globals, input: &Input, logout: bool) -> Result {
    let target = input.str("target").map(String::from);
    let credential = input.str("credential").map(String::from);
    if target.is_none() {
        require_interactive("Pass an integration ID or name when running without an interactive terminal")?;
    }
    if credential.is_none() {
        require_interactive("Pass a credential ID or label when running without an interactive terminal")?;
    }
    c.intro(if logout { "Log out of an account" } else { "Switch account" });
    let resolved = server(globals).await?;
    let client = resolved.client();
    let result = async {
        let integrations = load_integrations(&client).await?;
        let integration = choose_integration(c, &integrations, target.as_deref()).await?;
        let name = get_str(&integration, "name").unwrap_or("").to_owned();
        let action = if logout { "log out" } else { "switch to" };
        let id = choose_credential(c, &integration, action, credential.as_deref()).await?;
        c.start();
        let call = if logout {
            client.call("credential.remove", &[("credentialID", &id)], None, None).await
        } else {
            client.call("credential.activate", &[("credentialID", &id)], None, None).await
        };
        match call {
            Ok(_) => c.stop(&if logout { format!("Removed account from {name}") } else { format!("Switched account for {name}") }, 0),
            Err(error) => {
                c.stop(if logout { "Failed to remove credential" } else { "Failed to switch account" }, 1);
                return Err(error.into());
            }
        }
        c.outro("Done");
        Ok(0)
    }
    .await;
    resolved.close().await;
    result
}

fn stored(integration: &Json) -> Vec<&Json> {
    connections(integration).iter().filter(|c| get_str(c, "type") == Some("credential")).collect()
}

/// `chooseIntegration`: the one named, else one with stored credentials picked on the terminal.
async fn choose_integration(c: &Clack, integrations: &[Json], target: Option<&str>) -> Result<Json> {
    if let Some(target) = target {
        return resolve_integration(integrations, target).cloned();
    }
    let configured: Vec<Json> = integrations.iter().filter(|i| !stored(i).is_empty()).cloned().collect();
    if configured.is_empty() {
        return Err(Fail::error("No stored credentials found"));
    }
    let choices: Vec<Choice> = configured
        .iter()
        .map(|i| {
            let labels: Vec<&str> = stored(i).iter().filter_map(|c| get_str(c, "label")).collect();
            choice(get_str(i, "name").unwrap_or(""), Some(&labels.join(", ")))
        })
        .collect();
    let values: Vec<String> = configured.iter().map(|i| get_str(i, "id").unwrap_or("").to_owned()).collect();
    let Some(index) = prompt::autocomplete(c, "Select integration", &choices, &values, 8).await? else {
        // TODO(port): TS bug: a search that matches nothing submits `undefined`, which `resolveIntegration` crashes
        // on (a TypeError, in the JS engine's words); this says what happened instead.
        return Err(Fail::error("No integration selected"));
    };
    resolve_integration(&configured, &values[index]).cloned()
}

/// `chooseCredential`: the stored credential named (by ID, else by label), else one picked on the terminal.
async fn choose_credential(c: &Clack, integration: &Json, action: &str, target: Option<&str>) -> Result<String> {
    let name = get_str(integration, "name").unwrap_or("");
    let credentials = stored(integration);
    if credentials.is_empty() {
        let environment: Vec<&str> = connections(integration)
            .iter()
            .filter(|c| get_str(c, "type") == Some("env"))
            .filter_map(|c| get_str(c, "name"))
            .collect();
        if !environment.is_empty() && action == "log out" {
            return Err(Fail::error(format!(
                "{name} is authenticated through {}; unset the environment variable to disconnect",
                environment.join(", ")
            )));
        }
        return Err(Fail::error(format!("No stored credentials for {name}")));
    }
    if let Some(target) = target {
        if let Some(found) = credentials.iter().find(|c| get_str(c, "id") == Some(target)) {
            return Ok(get_str(found, "id").unwrap_or("").to_owned());
        }
        let matches: Vec<&&Json> = credentials
            .iter()
            .filter(|c| get_str(c, "label").is_some_and(|l| l.to_lowercase() == target.to_lowercase()))
            .collect();
        return match matches.len() {
            1 => Ok(get_str(matches[0], "id").unwrap_or("").to_owned()),
            0 => Err(Fail::error(format!("Credential not found for {name}: {target}"))),
            _ => Err(Fail::error(format!(
                "Credential label \"{target}\" is ambiguous. Use an ID: {}",
                matches.iter().filter_map(|c| get_str(c, "id")).collect::<Vec<_>>().join(", ")
            ))),
        };
    }
    let choices: Vec<Choice> = credentials
        .iter()
        .enumerate()
        .map(|(index, credential)| {
            let label = get_str(credential, "label").unwrap_or("");
            let label = if index == 0 { format!("{label} (active)") } else { label.to_owned() };
            choice(&label, get_str(credential, "id"))
        })
        .collect();
    let values: Vec<String> = credentials.iter().map(|c| get_str(c, "id").unwrap_or("").to_owned()).collect();
    let index = prompt::autocomplete(c, &format!("Select {name} account to {action}"), &choices, &values, 8).await?;
    // TODO(port): TS bug: a search that matches nothing submits `undefined`, sent as the credential ID "undefined" (no
    // credential: the server does nothing and the command reports success).
    Ok(index.map_or_else(|| "undefined".to_owned(), |index| values[index].clone()))
}
