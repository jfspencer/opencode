//! `opencode auth` (packages/cli/src/commands/handlers/auth/*.ts, ui/prompt.ts): integrations and credentials
//! through the server, with @clack/prompts 1.0.0-alpha.1's output (`intro`, `log`, `spinner`, `outro`, the
//! password prompt) reproduced in `Clack`. The interactive pickers (no integration or credential given on a
//! terminal) are TODO(port); the key prompt is ported.

use opencode_protocol::Json;

use super::client::Client;
use super::connection::{self, Args, Resolved};
use super::out::{Fail, Result, pretty, stderr, stdout};
use super::parse::{Globals, Input};
use crate::json::{self, Obj, get, get_str};

/// picocolors: colors when stdout is a terminal (and `NO_COLOR` is unset), as clack decides.
struct Clack {
    color: bool,
}

impl Clack {
    fn new() -> Self {
        let color = std::env::var_os("NO_COLOR").is_none()
            && (std::env::var_os("FORCE_COLOR").is_some()
                || (super::stdout_tty() && std::env::var("TERM").as_deref() != Ok("dumb"))
                || std::env::var_os("CI").is_some());
        Clack { color }
    }
    fn paint(&self, open: u8, close: u8, text: &str) -> String {
        if self.color { format!("\x1b[{open}m{text}\x1b[{close}m") } else { text.to_owned() }
    }
    fn gray(&self, text: &str) -> String {
        self.paint(90, 39, text)
    }
    fn red(&self, text: &str) -> String {
        self.paint(31, 39, text)
    }
    fn green(&self, text: &str) -> String {
        self.paint(32, 39, text)
    }
    fn yellow(&self, text: &str) -> String {
        self.paint(33, 39, text)
    }
    fn cyan(&self, text: &str) -> String {
        self.paint(36, 39, text)
    }
    fn dim(&self, text: &str) -> String {
        self.paint(2, 22, text)
    }
    fn intro(&self, title: &str) {
        stdout(&format!("{}  {title}\n", self.gray("┌")));
    }
    fn outro(&self, message: &str) {
        stdout(&format!("{}\n{}  {message}\n\n", self.gray("│"), self.gray("└")));
    }
    fn error(&self, message: &str) {
        let mut parts = vec![self.gray("│")];
        let mut lines = message.split('\n');
        let symbol = self.red("■");
        let first = lines.next().unwrap_or("");
        parts.push(if first.is_empty() { symbol } else { format!("{symbol}  {first}") });
        for line in lines {
            parts.push(if line.is_empty() { self.gray("│") } else { format!("{}  {line}", self.gray("│")) });
        }
        stdout(&format!("{}\n", parts.join("\n")));
    }
    /// `spinner().start(message)` (no frames are drawn: the operation is short).
    fn start(&self) {
        stdout(&format!("\x1b[?25l{}\n", self.gray("│")));
    }
    fn stop(&self, message: &str, code: u8) {
        let step = match code {
            0 => self.green("◇"),
            1 => self.red("■"),
            _ => self.red("▲"),
        };
        stdout(&format!("{step}  {message}\n\x1b[?25h"));
    }
}

/// `handlePromptErrors`: a failure is logged in clack's frame, exit 1.
fn prompt_errors(clack: &Clack, result: Result) -> Result {
    match result {
        Err(fail) if fail.name == "Cancelled" => {
            stdout(&format!("{}  {}\n\n", clack.gray("└"), clack.red("Cancelled")));
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

/// `answerForm` for the supplied answers (interactive fields are TODO(port)).
fn answer_form(fields: Option<&Json>, supplied: &[String]) -> Result<Option<Json>> {
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
        given.push((key.to_owned(), item[separator + 1..].to_owned()));
    }
    let Some(fields) = fields else { return Ok(None) };
    let mut answer = Obj::new();
    for field in fields {
        let key = get_str(field, "key").unwrap_or("");
        match given.iter().find(|(k, _)| k == key) {
            Some((_, value)) => {
                answer.str(key, value);
            }
            None if json::get_bool(field, "hidden") == Some(true) => {
                answer.opt(key, get(field, "default").cloned());
            }
            None => {
                require_interactive("Authentication form input requires an interactive terminal")?;
                // TODO(port): interactive form fields (text, select, confirm, multiselect, external steps).
                return Err(Fail::error("Authentication form input requires an interactive terminal"));
            }
        }
    }
    Ok(Some(answer.done()))
}

async fn login(clack: &Clack, globals: &Globals, input: &Input) -> Result {
    let target = input.str("target").map(String::from);
    if target.is_none() {
        require_interactive("Pass an integration ID or name when running without an interactive terminal")?;
    }
    clack.intro("Connect an integration");
    let resolved = server(globals).await?;
    let client = resolved.client();
    let result = async {
        let integrations = load_integrations(&client).await?;
        let Some(target) = &target else {
            // TODO(port): the integration picker (ui/integration-picker.ts).
            return Err(Fail::error("No authentication integrations are available"));
        };
        let integration = resolve_integration(&integrations, target)?.clone();
        let name = get_str(&integration, "name").unwrap_or("").to_owned();
        let methods = connect_methods(&integration);
        if methods.is_empty() {
            return Err(Fail::error(format!("{name} has no interactive login methods")));
        }
        let method = match input.str("method") {
            Some(method) => resolve_method(&methods, method)?,
            None if methods.len() == 1 => methods[0].clone(),
            None => {
                require_interactive("Pass --method when running without an interactive terminal")?;
                // TODO(port): the method picker.
                return Err(Fail::error("Pass --method when running without an interactive terminal"));
            }
        };
        let form = if get_str(&method, "type") == Some("command") { None } else { get(&method, "form") };
        let answer = answer_form(form, input.list("answer"))?;
        match get_str(&method, "type") {
            Some("key") => {
                let message = get_str(&method, "label").map(String::from).unwrap_or_else(|| format!("Enter your {name} API key"));
                require_interactive("API key input requires an interactive terminal")?;
                let key = password(clack, &message)?;
                clack.start();
                let mut body = Obj::new();
                body.str("key", &key).opt("answer", answer);
                let id = get_str(&integration, "id").unwrap_or("").to_owned();
                match client.call("integration.connect.key", &[("integrationID", &id)], Some(location()), Some(body.done())).await {
                    Ok(_) => clack.stop(&format!("Connected to {name}"), 0),
                    Err(error) => {
                        clack.stop("Authentication failed", 1);
                        return Err(error.into());
                    }
                }
            }
            // TODO(port): OAuth, external and command logins (browser flows, polling).
            _ => return Err(Fail::error(format!("{name} needs a login method this build does not support"))),
        }
        clack.outro("Done");
        Ok(0)
    }
    .await;
    resolved.close().await;
    result
}

async fn account(clack: &Clack, globals: &Globals, input: &Input, logout: bool) -> Result {
    let target = input.str("target").map(String::from);
    let credential = input.str("credential").map(String::from);
    if target.is_none() {
        require_interactive("Pass an integration ID or name when running without an interactive terminal")?;
    }
    if credential.is_none() {
        require_interactive("Pass a credential ID or label when running without an interactive terminal")?;
    }
    clack.intro(if logout { "Log out of an account" } else { "Switch account" });
    let resolved = server(globals).await?;
    let client = resolved.client();
    let result = async {
        let integrations = load_integrations(&client).await?;
        // TODO(port): the integration and account pickers on a terminal.
        let integration = resolve_integration(&integrations, target.as_deref().unwrap_or(""))?.clone();
        let name = get_str(&integration, "name").unwrap_or("").to_owned();
        let stored: Vec<&Json> = connections(&integration).iter().filter(|c| get_str(c, "type") == Some("credential")).collect();
        if stored.is_empty() {
            let environment: Vec<&str> = connections(&integration)
                .iter()
                .filter(|c| get_str(c, "type") == Some("env"))
                .filter_map(|c| get_str(c, "name"))
                .collect();
            if !environment.is_empty() && logout {
                return Err(Fail::error(format!(
                    "{name} is authenticated through {}; unset the environment variable to disconnect",
                    environment.join(", ")
                )));
            }
            return Err(Fail::error(format!("No stored credentials for {name}")));
        }
        let target = credential.unwrap_or_default();
        let id = match stored.iter().find(|c| get_str(c, "id") == Some(&target)) {
            Some(found) => get_str(found, "id").unwrap_or("").to_owned(),
            None => {
                let matches: Vec<&&Json> = stored
                    .iter()
                    .filter(|c| get_str(c, "label").is_some_and(|l| l.to_lowercase() == target.to_lowercase()))
                    .collect();
                match matches.len() {
                    1 => get_str(matches[0], "id").unwrap_or("").to_owned(),
                    0 => return Err(Fail::error(format!("Credential not found for {name}: {target}"))),
                    _ => {
                        return Err(Fail::error(format!(
                            "Credential label \"{target}\" is ambiguous. Use an ID: {}",
                            matches.iter().filter_map(|c| get_str(c, "id")).collect::<Vec<_>>().join(", ")
                        )));
                    }
                }
            }
        };
        clack.start();
        let call = if logout {
            client.call("credential.remove", &[("credentialID", &id)], None, None).await
        } else {
            client.call("credential.activate", &[("credentialID", &id)], None, None).await
        };
        match call {
            Ok(_) => clack.stop(&if logout { format!("Removed account from {name}") } else { format!("Switched account for {name}") }, 0),
            Err(error) => {
                clack.stop(if logout { "Failed to remove credential" } else { "Failed to switch account" }, 1);
                return Err(error.into());
            }
        }
        clack.outro("Done");
        Ok(0)
    }
    .await;
    resolved.close().await;
    result
}

// --- the password prompt (@clack/prompts `password`) on a terminal ---

struct Raw {
    saved: libc::termios,
}

impl Raw {
    fn enable() -> Option<Raw> {
        // SAFETY: termios calls on stdin with a zeroed struct they fill.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut saved) != 0 {
                return None;
            }
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            raw.c_oflag = saved.c_oflag;
            libc::tcsetattr(0, libc::TCSANOW, &raw);
            Some(Raw { saved })
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: restores the settings read in `enable`.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.saved);
        }
    }
}

fn password(clack: &Clack, message: &str) -> Result<String> {
    use std::io::Read;
    let raw = Raw::enable();
    let mut value = String::new();
    let mut previous_lines = 0usize;
    let frame = |state: &str, value: &str, error: Option<&str>| -> String {
        let masked = "▪".repeat(value.chars().count());
        let symbol = match state {
            "error" => clack.yellow("▲"),
            "submit" => clack.green("◇"),
            "cancel" => clack.red("■"),
            _ => clack.cyan("◆"),
        };
        let title = format!("{}\n{symbol}  {message}\n", clack.gray("│"));
        match state {
            "error" => format!(
                "{}\n{}  {masked}\n{}  {}\n",
                title.trim(),
                clack.yellow("│"),
                clack.yellow("└"),
                clack.yellow(error.unwrap_or(""))
            ),
            "submit" => format!("{title}{}  {}", clack.gray("│"), clack.dim(&masked)),
            "cancel" => format!("{title}{}  {}", clack.gray("│"), clack.dim(&masked)),
            _ => format!("{title}{}  {masked}{}\n{}\n", clack.cyan("│"), if clack.color { "\x1b[7m \x1b[27m" } else { "_" }, clack.cyan("└")),
        }
    };
    let draw = |text: &str, previous: &mut usize| {
        if *previous > 0 {
            stdout(&format!("\x1b[999D\x1b[{previous}A\x1b[J"));
        } else {
            stdout("\x1b[999D\x1b[J");
        }
        stdout(text);
        *previous = text.matches('\n').count();
    };
    stdout("\x1b[?25l");
    draw(&frame("active", &value, None), &mut previous_lines);
    let mut stdin = std::io::stdin();
    let mut byte = [0u8; 1];
    loop {
        if stdin.read(&mut byte).unwrap_or(0) == 0 {
            drop(raw);
            return Err(Fail::named("Cancelled", "Cancelled"));
        }
        match byte[0] {
            b'\r' | b'\n' => {
                if value.is_empty() {
                    draw(&frame("error", &value, Some("Required")), &mut previous_lines);
                    continue;
                }
                draw(&frame("submit", &value, None), &mut previous_lines);
                stdout("\x1b[?25h\n");
                drop(raw);
                return Ok(value);
            }
            0x03 | 0x1b => {
                draw(&frame("cancel", &value, None), &mut previous_lines);
                stdout("\x1b[?25h\n");
                drop(raw);
                return Err(Fail::named("Cancelled", "Cancelled"));
            }
            0x7f | 0x08 => {
                value.pop();
            }
            byte if byte >= 0x20 => value.push(byte as char),
            _ => {}
        }
        draw(&frame("active", &value, None), &mut previous_lines);
    }
}
