//! The `opencode.tools` plugin's Code Mode tools (packages/core/src/tool/plugin/opencode.ts): renaming and moving
//! sessions, the project's worktrees, and the model search, under the `opencode` namespace. Their definitions (the
//! effective names `opencode_<name>`, the input and output schemas, the options) are recorded from the reference in
//! `definitions/opencode.json`.
//!
//! Each input is decoded as its Effect Schema decodes it (every issue reported, in key order, with the Standard
//! Schema formatter's messages, unknown keys dropped), and a failure carries the curated message TS maps it to.
//! Permission failures keep their own message (`toSessionError` unwraps them), and a decline stops the turn as it
//! does for any tool.
//!
//! TODO(port): `FileAccess.resolve` / `authorizeExternal` are reproduced here for directories until the file tools'
//! `tool::file_access` reaches dev.

use std::sync::Arc;

use opencode_protocol::Json;

use super::{CodeModeExecution, Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::json::{self, Obj};
use crate::permission::Ask;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    SessionRename,
    SessionMove,
    WorktreeCreate,
    WorktreeList,
    WorktreeRemove,
    Models,
}

pub struct OpenCode {
    /// `None` for a recorded tool whose execution is not ported (the schedule tools).
    kind: Option<Kind>,
    definition: Definition,
    path: String,
    output: Json,
    pinned: bool,
    permission: String,
}

/// The plugin's tools, in registration order.
pub fn tools() -> Vec<Arc<dyn Tool>> {
    recorded(include_str!("definitions/opencode.json"))
}

/// Code Mode tools recorded from the reference (`{tools: [{name, tool, options, description, parameters, output}]}`),
/// in registration order; those this module does not implement fail as not yet supported.
pub fn recorded(text: &str) -> Vec<Arc<dyn Tool>> {
    let recorded = json::parse(text).expect("recorded tools are JSON");
    json::get(&recorded, "tools")
        .and_then(json::array)
        .unwrap_or_default()
        .iter()
        .map(|tool| {
            let name = json::get_str(tool, "name").unwrap_or_default().to_owned();
            let short = json::get_str(tool, "tool").unwrap_or_default();
            let options = json::get(tool, "options").cloned().unwrap_or_else(json::object);
            let kind = match short {
                "session_rename" => Some(Kind::SessionRename),
                "session_move" => Some(Kind::SessionMove),
                "worktree_create" => Some(Kind::WorktreeCreate),
                "worktree_list" => Some(Kind::WorktreeList),
                "worktree_remove" => Some(Kind::WorktreeRemove),
                "models" => Some(Kind::Models),
                _ => None,
            };
            Arc::new(OpenCode {
                kind,
                path: format!("{}.{short}", json::get_str(&options, "namespace").unwrap_or("opencode")),
                pinned: json::get_bool(&options, "pinned").unwrap_or(false),
                permission: json::get_str(&options, "permission").unwrap_or(&name).to_owned(),
                output: json::get(tool, "output").cloned().unwrap_or_else(json::object),
                definition: Definition {
                    description: json::get_str(tool, "description").unwrap_or_default().to_owned(),
                    parameters: json::get(tool, "parameters").cloned().unwrap_or_else(json::object),
                    name,
                },
            }) as Arc<dyn Tool>
        })
        .collect()
}

impl Tool for OpenCode {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn permission(&self) -> &str {
        &self.permission
    }

    fn code_mode(&self) -> bool {
        true
    }

    fn code_mode_path(&self) -> String {
        self.path.clone()
    }

    fn output_schema(&self) -> Option<&Json> {
        Some(&self.output)
    }

    fn pinned(&self) -> bool {
        self.pinned
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(async move { self.run(context, Some(input)).await.map(|(output, _)| output) })
    }

    fn execute_code_mode<'a>(&'a self, context: Context, input: Option<Json>) -> CodeModeExecution<'a> {
        Box::pin(self.run(context, input))
    }
}

impl OpenCode {
    async fn run(&self, context: Context, input: Option<Json>) -> Result<(Output, Option<Json>), Failure> {
        let Some(kind) = self.kind else {
            return Err(curated(format!("Tool not yet supported by the Rust port: {}", self.definition.name)));
        };
        let decoded = decode(&self.definition.name, schema(kind), input.as_ref()).map_err(Failure::Error)?;
        let (output, content) = match kind {
            Kind::SessionRename => session_rename(&context, &decoded)?,
            Kind::SessionMove => session_move(&context, &decoded).await?,
            Kind::WorktreeCreate => worktree_create(&context, &decoded).await?,
            Kind::WorktreeList => worktree_list(&context)?,
            Kind::WorktreeRemove => worktree_remove(&context, &decoded).await?,
            Kind::Models => models(&context, &decoded)?,
        };
        // `normalizeContent`: the tool's text, else the output as JSON.
        let text = content.unwrap_or_else(|| json::text(&output));
        let mut part = Obj::new();
        part.str("type", "text").str("text", &text);
        Ok((Output { content: vec![part.done()], metadata: None }, Some(output)))
    }
}

// --- Input decoding (tool/runtime.ts `decodeInput`) ----------------------------------------------------------------

enum Check {
    MinLength(usize),
    StartsWith(&'static str),
    Between(f64, f64),
    AtLeast(f64),
}

enum Type {
    /// A string (`expected` names it when it is not one), trimmed first when `trim`.
    String { expected: &'static str, trim: bool },
    Boolean,
    Int,
    Literals(&'static [&'static str]),
}

struct Field {
    key: &'static str,
    optional: bool,
    ty: Type,
    checks: &'static [Check],
}

/// `None` is `Schema.Struct({})`, which takes any value but `null` and `undefined`.
fn schema(kind: Kind) -> Option<&'static [Field]> {
    const SESSION_ID: Field = Field {
        key: "sessionID",
        optional: true,
        ty: Type::String { expected: "Session.ID", trim: false },
        checks: &[Check::StartsWith("ses")],
    };
    const DIRECTORY: Field =
        Field { key: "directory", optional: false, ty: Type::String { expected: "string", trim: false }, checks: &[Check::MinLength(1)] };
    const STRING: Type = Type::String { expected: "string", trim: false };
    match kind {
        Kind::SessionRename => Some(&[SESSION_ID, Field { key: "title", optional: false, ty: STRING, checks: &[Check::MinLength(1)] }]),
        Kind::SessionMove => Some(&[SESSION_ID, DIRECTORY]),
        Kind::WorktreeCreate => Some(&[Field {
            key: "name",
            optional: false,
            ty: Type::String { expected: "string", trim: true },
            checks: &[Check::MinLength(1)],
        }]),
        Kind::WorktreeList => None,
        Kind::WorktreeRemove => Some(&[DIRECTORY, Field { key: "force", optional: true, ty: Type::Boolean, checks: &[] }]),
        Kind::Models => Some(&[
            Field { key: "query", optional: true, ty: STRING, checks: &[] },
            Field { key: "provider", optional: true, ty: STRING, checks: &[] },
            Field { key: "type", optional: true, ty: Type::Literals(&["chat", "image"]), checks: &[] },
            Field { key: "all", optional: true, ty: Type::Boolean, checks: &[] },
            Field { key: "limit", optional: true, ty: Type::Int, checks: &[Check::Between(1.0, 100.0)] },
            Field { key: "offset", optional: true, ty: Type::Int, checks: &[Check::AtLeast(0.0)] },
        ]),
    }
}

/// The decoded input, or the tool error that restates the arguments (`formatInputIssues`).
fn decode(tool: &str, fields: Option<&[Field]>, input: Option<&Json>) -> Result<Json, ToolError> {
    let mut issues: Vec<(String, String)> = Vec::new();
    let decoded = match (fields, input) {
        (None, None | Some(Json::Null)) => {
            issues.push((String::new(), "Expected object | array".into()));
            Json::Null
        }
        (None, Some(value)) => value.clone(),
        (Some(fields), Some(Json::Object(object))) => {
            let mut out = Obj::new();
            for field in fields {
                match object.get(field.key) {
                    None if field.optional => {}
                    None => issues.push((field.key.into(), "Missing key".into())),
                    Some(value) => match decode_field(field, value) {
                        Ok(value) => {
                            out.set(field.key, value);
                        }
                        Err(messages) => issues.extend(messages.into_iter().map(|m| (field.key.to_owned(), m))),
                    },
                }
            }
            out.done()
        }
        (Some(_), _) => {
            issues.push((String::new(), "Expected object".into()));
            Json::Null
        }
    };
    if issues.is_empty() {
        return Ok(decoded);
    }
    let mut details: Vec<String> = issues
        .iter()
        .take(5)
        .map(|(path, message)| format!("- {}: {message}", if path.is_empty() { "root" } else { path }))
        .collect();
    if issues.len() > 5 {
        let more = issues.len() - 5;
        details.push(format!("- ...and {more} more {}", if more == 1 { "issue" } else { "issues" }));
    }
    Err(ToolError::new(format!(
        "Invalid arguments for tool \"{tool}\":\n{}\n\nArguments provided:\n{}\n\nUpdate the arguments and call the tool again.",
        details.join("\n"),
        crate::codemode::pretty(input)
    )))
}

/// One key: its type, then every check (Effect runs them all under `errors: "all"`).
fn decode_field(field: &Field, value: &Json) -> Result<Json, Vec<String>> {
    let decoded = match (&field.ty, value) {
        (Type::String { trim, .. }, Json::String(text)) => {
            Json::String(if *trim { crate::text::trim(text).to_owned() } else { text.clone() })
        }
        (Type::String { expected, .. }, _) => return Err(vec![format!("Expected {expected}")]),
        (Type::Boolean, Json::Bool(_)) => value.clone(),
        (Type::Boolean, _) => return Err(vec!["Expected boolean".into()]),
        (Type::Int, Json::Number(_)) => value.clone(),
        (Type::Int, _) => return Err(vec!["Expected number".into()]),
        (Type::Literals(options), Json::String(text)) if options.contains(&text.as_str()) => value.clone(),
        (Type::Literals(options), _) => {
            return Err(vec![format!("Expected {}", options.iter().map(|o| format!("\"{o}\"")).collect::<Vec<_>>().join(" | "))]);
        }
    };
    let mut messages = Vec::new();
    if let (Type::Int, Json::Number(n)) = (&field.ty, &decoded) {
        if !(n.fract() == 0.0 && n.abs() <= 9007199254740991.0) {
            messages.push("Expected an integer".to_owned());
        }
    }
    for check in field.checks {
        match (check, &decoded) {
            (Check::MinLength(min), Json::String(text)) if text.encode_utf16().count() < *min => {
                messages.push(format!("Expected a value with a length of at least {min}"));
            }
            (Check::StartsWith(prefix), Json::String(text)) if !text.starts_with(prefix) => {
                messages.push(format!("Expected a string starting with \"{prefix}\""));
            }
            (Check::Between(min, max), Json::Number(n)) if !(*n >= *min && *n <= *max) => {
                messages.push(format!("Expected a value between {min} and {max}"));
            }
            (Check::AtLeast(min), Json::Number(n)) if !(*n >= *min) => {
                messages.push(format!("Expected a value greater than or equal to {min}"));
            }
            _ => {}
        }
    }
    if messages.is_empty() { Ok(decoded) } else { Err(messages) }
}

// --- Failures ------------------------------------------------------------------------------------------------------

/// `new ToolFailure({ message, error })` as `toSessionError` reads it: the cause's own message when it has one (a
/// permission refusal), else the curated one. A decline passes through.
fn wrap(message: String, failure: Failure) -> Failure {
    match failure {
        Failure::Error(error) if error.kind == "permission.rejected" && !error.message.is_empty() => Failure::Error(error),
        declined @ Failure::Declined { .. } => declined,
        Failure::Error(_) => Failure::Error(ToolError::new(message)),
    }
}

fn curated(message: impl Into<String>) -> Failure {
    Failure::Error(ToolError::new(message))
}

fn string<'a>(input: &'a Json, key: &str) -> Option<&'a str> {
    json::get_str(input, key)
}

type Done = Result<(Json, Option<String>), Failure>;

// --- Sessions ------------------------------------------------------------------------------------------------------

fn session_rename(context: &Context, input: &Json) -> Done {
    let session_id = string(input, "sessionID").unwrap_or(&context.session_id).to_owned();
    let title = crate::text::trim(string(input, "title").unwrap_or_default()).to_owned();
    if title.is_empty() {
        return Err(curated("Session title must not be empty"));
    }
    if crate::session::rename(&context.app, &session_id, &title).is_err() {
        return Err(curated(format!("Unable to rename session {session_id}")));
    }
    let mut output = Obj::new();
    output.str("sessionID", &session_id).str("title", &title);
    Ok((output.done(), Some(format!("Renamed session {session_id} to {title}."))))
}

async fn session_move(context: &Context, input: &Json) -> Done {
    let written = string(input, "directory").unwrap_or_default().to_owned();
    let failed = || format!("Unable to move session to {written}");
    let session_id = string(input, "sessionID").unwrap_or(&context.session_id).to_owned();
    let Ok(session) = crate::session::require(&context.app.db.lock().unwrap(), &session_id) else { return Err(curated(failed())) };
    let directory = resolve_path(&session.location.directory, crate::text::trim(&written));
    // The destination becomes the session's directory, so leaving the project needs the same approval as reaching
    // outside it. The project's own worktrees don't.
    let target = resolve_directory(&context.location, &directory);
    let worktrees = match &target.external {
        Some(_) => crate::worktree::list(&context.app, &context.location.project.id).unwrap_or_default(),
        None => Vec::new(),
    };
    if !worktrees.iter().any(|worktree| crate::project::contains(&worktree.directory, &directory)) {
        authorize_external(context, &target).await.map_err(|failure| wrap(failed(), failure))?;
    }
    let mut request = Obj::new();
    request.str("directory", &directory).str("delivery", "steer");
    if crate::session::moves::request(&context.app, &session_id, &request.done()).await.is_err() {
        return Err(curated(failed()));
    }
    let mut output = Obj::new();
    output.str("sessionID", &session_id).str("directory", &directory);
    Ok((output.done(), Some(format!("Moved session {session_id} to {directory}."))))
}

// --- Worktrees -----------------------------------------------------------------------------------------------------

async fn worktree_create(context: &Context, input: &Json) -> Done {
    let name = string(input, "name").unwrap_or_default().to_owned();
    let created = crate::worktree::create(
        &context.app,
        crate::worktree::CreateInput {
            project_id: context.location.project.id.clone(),
            name: Some(name.clone()),
            directory: None,
            from: None,
            branch: None,
        },
    )
    .await
    .map_err(|_| curated(format!("Unable to create worktree {name}")))?;
    let mut output = Obj::new();
    output.str("directory", &created);
    Ok((output.done(), Some(format!("Created worktree in {created}."))))
}

fn worktree_list(context: &Context) -> Done {
    let worktrees = crate::worktree::list(&context.app, &context.location.project.id)
        .map_err(|_| curated("Unable to list worktrees"))?;
    let mut output = Obj::new();
    output.set("worktrees", Json::Array(worktrees.iter().map(|w| w.to_json()).collect()));
    Ok((output.done(), None))
}

async fn worktree_remove(context: &Context, input: &Json) -> Done {
    let written = string(input, "directory").unwrap_or_default().to_owned();
    // Removing a worktree deletes files, so it requires the same approval as editing them.
    let target = resolve_directory(&context.location, &resolve_path(&context.location.directory, &written));
    let ask = Ask { action: "edit".into(), resources: vec![target.resource.clone()], save: Some(vec!["*".into()]), metadata: None };
    context.assert(ask).await.map_err(|failure| wrap(format!("Unable to remove worktree {written}"), failure))?;
    let force = json::get_bool(input, "force").unwrap_or(false);
    let directory = target.absolute;
    if let Err(error) = crate::worktree::remove(&context.app, &context.location.project.id, &directory, force).await {
        return Err(curated(remove_failure(&directory, error)));
    }
    let mut output = Obj::new();
    output.str("directory", &directory).set("removed", Json::Bool(true));
    Ok((output.done(), Some(format!("Removed worktree {directory}."))))
}

/// `removeFailure`: worktree errors carry no model-facing message, and git's own text suggests forcing removal.
fn remove_failure(directory: &str, error: crate::worktree::Error) -> String {
    use crate::worktree::Error;
    match error {
        Error::DirectoryUnavailable(_) => {
            format!("Unable to remove worktree {directory}: the directory does not exist or is not a directory.")
        }
        Error::InvalidDirectory(_) => format!(
            "Unable to remove {directory}: it is not a worktree of this project. Use worktree_list to see the project's worktrees."
        ),
        Error::StrategyUnavailable(strategy) => {
            format!("Unable to remove worktree {directory}: its worktree strategy {strategy} is not available.")
        }
        Error::Operation { force_required: Some(true), .. } => {
            format!("Unable to remove worktree {directory}: it has modified or untracked files.")
        }
        _ => format!("Unable to remove worktree {directory}."),
    }
}

// --- Models --------------------------------------------------------------------------------------------------------

fn models(context: &Context, input: &Json) -> Done {
    let offset = json::get_num(input, "offset").unwrap_or(0.0) as usize;
    let limit = json::get_num(input, "limit").unwrap_or(20.0) as usize;
    let Ok(session) = crate::session::require(&context.app.db.lock().unwrap(), &context.session_id) else {
        return Err(curated("Unable to list models"));
    };
    let own = json::get(&session.info, "model").and_then(|m| json::get_str(m, "providerID")).map(str::to_owned);
    let terms: Vec<String> =
        string(input, "query").map(|q| q.to_lowercase().split_whitespace().map(str::to_owned).collect()).unwrap_or_default();
    let inventory = context.location.inventory();
    let names: Vec<(String, String)> = inventory
        .catalog
        .available()
        .map(|p| json::of(p))
        .map(|p| (json::get_str(&p, "id").unwrap_or_default().to_owned(), json::get_str(&p, "name").unwrap_or_default().to_owned()))
        .collect();
    let name_of = |id: &str| names.iter().find(|(key, _)| key == id).map(|(_, name)| name.clone());
    let ty = string(input, "type").unwrap_or("chat");
    let listed: Vec<Json> = if ty == "chat" {
        inventory.available_models().map(json::of).collect()
    } else {
        inventory.catalog.other.iter().map(json::of).filter(|m| json::get_str(m, "type") == Some(ty)).collect()
    };
    let provider = string(input, "provider").map(str::to_lowercase);
    let provider_of = |m: &Json| json::get_str(m, "providerID").unwrap_or_default().to_owned();
    let released = |m: &Json| json::get(m, "time").and_then(|t| json::get_num(t, "released")).unwrap_or(0.0);
    let mut matching: Vec<Json> = listed
        .into_iter()
        .filter(|m| {
            let id = provider_of(m);
            provider.as_ref().is_none_or(|p| id.to_lowercase() == *p || name_of(&id).is_some_and(|n| n.to_lowercase() == *p))
        })
        .filter(|m| {
            let text = format!(
                "{}/{} {}",
                provider_of(m),
                json::get_str(m, "id").unwrap_or_default(),
                json::get_str(m, "name").unwrap_or_default()
            )
            .to_lowercase();
            terms.iter().all(|term| text.contains(term.as_str()))
        })
        .collect();
    let is_own = |m: &Json| own.as_deref() == Some(provider_of(m).as_str());
    matching.sort_by(|a, b| {
        is_own(b)
            .cmp(&is_own(a))
            .then_with(|| crate::collate::compare(&provider_of(a), &provider_of(b)))
            .then_with(|| released(b).partial_cmp(&released(a)).unwrap_or(std::cmp::Ordering::Equal))
    });
    let all = json::get_bool(input, "all").unwrap_or(false);
    let mut families: Vec<(String, String)> = Vec::new();
    matching.retain(|m| {
        let Some(family) = json::get_str(m, "family").filter(|_| !all) else { return true };
        let key = (provider_of(m), family.to_owned());
        if families.contains(&key) {
            return false;
        }
        families.push(key);
        true
    });
    let page: Vec<&Json> = matching.iter().skip(offset).take(limit).collect();
    let mut providers: Vec<String> = Vec::new();
    for m in &page {
        let id = provider_of(m);
        if !providers.contains(&id) {
            providers.push(id);
        }
    }
    let providers: Vec<Json> = providers
        .iter()
        .map(|id| {
            let models: Vec<Json> = page
                .iter()
                .filter(|m| provider_of(m) == *id)
                .map(|m| {
                    let variants = json::get(m, "variants")
                        .and_then(json::array)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|v| json::get(v, "id").cloned())
                        .collect();
                    let mut entry = Obj::new();
                    entry.str("id", &format!("{id}/{}", json::get_str(m, "id").unwrap_or_default()));
                    entry.set("name", json::get(m, "name").cloned().unwrap_or(Json::Null));
                    entry.set("released", Json::Number(released(m)));
                    entry.set("variants", Json::Array(variants));
                    entry.set("cost", json::get(m, "cost").cloned().unwrap_or(Json::Array(Vec::new())));
                    entry.set("status", json::get(m, "status").cloned().unwrap_or(Json::Null));
                    entry.done()
                })
                .collect();
            let mut provider = Obj::new();
            provider.str("id", id).str("name", &name_of(id).unwrap_or_else(|| id.clone()));
            provider.set("models", Json::Array(models));
            provider.done()
        })
        .collect();
    let end = offset + limit;
    let mut output = Obj::new();
    output.set("providers", Json::Array(providers)).set("total", Json::Number(matching.len() as f64));
    output.set("next", if end < matching.len() { Json::Number(end as f64) } else { Json::Null });
    Ok((output.done(), None))
}

// --- File access for directories (file-access.ts) ------------------------------------------------------------------

/// `FileAccess.Target` of a directory.
struct Target {
    absolute: String,
    resource: String,
    /// `(resource, save)` of the external directory approval, for a target outside the location.
    external: Option<(String, String)>,
}

/// `FileAccess.resolvePath`: a leading `~` expanded, then resolved against `directory`.
fn resolve_path(directory: &str, input: &str) -> String {
    let home = crate::inventory::config::Options::get().home.clone();
    let expanded = match input {
        "~" => home,
        _ => match input.strip_prefix("~/") {
            Some(rest) => format!("{home}/{rest}"),
            None => input.to_owned(),
        },
    };
    crate::fs::resolve(directory, &expanded)
}

/// Symlinks followed through the part of the path that exists.
fn real_path(target: &str) -> String {
    match std::fs::canonicalize(target) {
        Ok(real) => real.to_string_lossy().into_owned(),
        Err(_) => match target.rsplit_once('/') {
            Some((parent, name)) if !parent.is_empty() => crate::project::normalize(&format!("{}/{name}", real_path(parent))),
            Some(("", name)) if !name.is_empty() => format!("/{name}"),
            _ => target.to_owned(),
        },
    }
}

fn inside(target: &str, directory: &str, worktree: &str) -> bool {
    crate::project::contains(directory, target) || (worktree != "/" && crate::project::contains(worktree, target))
}

/// `FileAccess.resolve({ path, kind: "directory" })`.
fn resolve_directory(location: &crate::location::Location, absolute: &str) -> Target {
    let real = |p: &str| if location.workspace_id.is_some() { p.to_owned() } else { real_path(p) };
    let canonical = real(absolute);
    let worktree = crate::project::normalize(&location.project.directory);
    if inside(absolute, &location.directory, &worktree) && inside(&canonical, &real(&location.directory), &real(&worktree)) {
        let relative = crate::text::relative(&location.directory, absolute);
        return Target { absolute: absolute.to_owned(), resource: if relative.is_empty() { ".".into() } else { relative }, external: None };
    }
    // Escaped so that a `*` or `?` in a directory name stays literal in the saved approval.
    let root = project_root(&canonical).unwrap_or_else(|| canonical.clone());
    let escaped: String = root.chars().map(|c| if c == '*' || c == '?' { format!("[{c}]") } else { c.to_string() }).collect();
    Target { absolute: absolute.to_owned(), external: Some((join_star(&canonical), join_star(&escaped))), resource: canonical }
}

/// `path.join(directory, "*")`.
fn join_star(directory: &str) -> String {
    if directory.ends_with('/') { format!("{directory}*") } else { format!("{directory}/*") }
}

/// `Project.root`: the nearest directory with a `.git` or `.hg`.
fn project_root(directory: &str) -> Option<String> {
    let mut current = directory.to_owned();
    loop {
        if [".git", ".hg"].iter().any(|marker| std::path::Path::new(&format!("{current}/{marker}")).exists()) {
            return Some(current);
        }
        let parent = match current.rsplit_once('/') {
            Some(("", _)) => "/".to_owned(),
            Some((parent, _)) => parent.to_owned(),
            None => return None,
        };
        if parent == current {
            return None;
        }
        current = parent;
    }
}

/// `FileAccess.authorizeExternal` for one target.
async fn authorize_external(context: &Context, target: &Target) -> Result<(), Failure> {
    let Some((resource, save)) = &target.external else { return Ok(()) };
    let ask = Ask {
        action: "external_directory".into(),
        resources: vec![resource.clone()],
        save: Some(vec![save.clone()]),
        metadata: None,
    };
    context.assert(ask).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues(kind: Kind, input: Option<&str>) -> String {
        let value = input.map(|text| json::parse(text).unwrap());
        match decode("tool", schema(kind), value.as_ref()) {
            Ok(decoded) => format!("ok {}", json::text(&decoded)),
            Err(error) => error.message.lines().filter(|l| l.starts_with("- ")).collect::<Vec<_>>().join("; "),
        }
    }

    /// The messages recorded from the reference's schemas.
    #[test]
    fn input_issues() {
        assert_eq!(issues(Kind::SessionRename, None), "- root: Expected object");
        assert_eq!(issues(Kind::SessionRename, Some("{}")), "- title: Missing key");
        assert_eq!(issues(Kind::SessionRename, Some(r#"{"title":"a","sessionID":5}"#)), "- sessionID: Expected Session.ID");
        assert_eq!(
            issues(Kind::SessionMove, Some(r#"{"title":"a","sessionID":"x"}"#)),
            "- sessionID: Expected a string starting with \"ses\"; - directory: Missing key"
        );
        assert_eq!(issues(Kind::WorktreeCreate, Some(r#"{"name":" a "}"#)), r#"ok {"name":"a"}"#);
        assert_eq!(issues(Kind::WorktreeCreate, Some(r#"{"name":"   "}"#)), "- name: Expected a value with a length of at least 1");
        assert_eq!(issues(Kind::WorktreeRemove, Some(r#"{"directory":"a","force":"x"}"#)), "- force: Expected boolean");
        assert_eq!(issues(Kind::WorktreeList, None), "- root: Expected object | array");
        assert_eq!(issues(Kind::WorktreeList, Some("5")), "ok 5");
        assert_eq!(issues(Kind::Models, Some(r#"{"limit":1.5}"#)), "- limit: Expected an integer");
        assert_eq!(issues(Kind::Models, Some(r#"{"limit":1e20}"#)), "- limit: Expected an integer; - limit: Expected a value between 1 and 100");
        assert_eq!(
            issues(Kind::Models, Some(r#"{"query":5,"provider":6,"type":"x","all":1,"limit":0,"offset":-1}"#)),
            "- query: Expected string; - provider: Expected string; - type: Expected \"chat\" | \"image\"; - all: Expected boolean; - limit: Expected a value between 1 and 100; - ...and 1 more issue"
        );
    }
}
