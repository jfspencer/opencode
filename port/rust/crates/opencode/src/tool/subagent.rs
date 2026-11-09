//! The subagent tool (packages/core/src/tool/plugin/subagent.ts): runs an agent in a child session, in the
//! foreground (the call blocks until the child's job finishes or is moved to the background) or in the background
//! (the parent is notified when it finishes, [`crate::session::subagent`]). A `sessionID` continues an existing
//! child, switching its agent and model when they change.
//!
//! The description lists the subagents the session's agent may call ([`describe`], the plugin's `context` and
//! `generate` hooks).

use std::sync::Arc;

use opencode_protocol::Json;
use opencode_protocol::types as t;

use crate::inventory::Inventory;
use crate::job::{Blocked, Status};
use crate::json::{self, Obj};
use crate::session::subagent::{self as jobs, NO_TEXT, PREAMBLE, Spawned};
use crate::session::{self, CreateInput};
use crate::tool::{Context, Definition, Execution, Failure, Output, Snapshot, Tool, ToolError};

pub const NAME: &str = "subagent";

/// The tool's own description (`description`), before the subagent list.
const DESCRIPTION: &str = "Spawns an agent in a child session to work on the specified task.
The output includes a sessionID you can pass back later to continue that specific conversation with the subagent.
New child sessions start with fresh context, so include all relevant context and instructions when you don't pass a sessionID.
Foreground (default) runs the subagent to completion and returns its final response.
Background mode (background=true) launches it asynchronously and returns immediately; you are notified when it finishes.
Use background only for independent work that can run while you continue elsewhere.";

pub struct Subagent {
    definition: Definition,
}

impl Subagent {
    /// The registered tool: the recorded schema with the tool's own description.
    pub fn new(recorded: Definition) -> Self {
        Self { definition: Definition { description: DESCRIPTION.to_owned(), ..recorded } }
    }
}

/// The subagent tool's `context` hook: the description lists the subagents the session's agent may call (not
/// primary, not hidden, not denied by the agent's rules), by ID.
pub fn describe(snapshot: &mut Snapshot, inventory: &Inventory, agent: &str) {
    let Some(slot) = snapshot.tools.iter_mut().find(|tool| tool.definition().name == NAME) else { return };
    let Some(selected) = inventory.agent(agent) else { return };
    let rules: Vec<Json> = selected.permissions.0.iter().map(json::of).collect();
    let mut available: Vec<&t::AgentInfo> = inventory
        .agents
        .iter()
        .filter(|a| !matches!(a.mode, t::AgentInfoMode::Primary) && !a.hidden)
        .filter(|a| crate::permission::evaluate(NAME, &a.id.0, &rules) != "deny")
        .collect();
    if available.is_empty() {
        return;
    }
    available.sort_by(|a, b| crate::collate::compare(&a.id.0, &b.id.0));
    let mut lines = vec![slot.definition().description.clone(), String::new(), "Available subagents:".to_owned()];
    lines.extend(available.iter().map(|a| {
        let description =
            a.description.as_deref().unwrap_or("This subagent should only be called when explicitly requested.");
        format!("- {}: {description}", a.id.0)
    }));
    let definition = Definition { description: lines.join("\n"), ..slot.definition().clone() };
    *slot = Arc::new(Subagent { definition });
}

fn failure(message: impl Into<String>) -> Failure {
    Failure::Error(ToolError::new(message))
}

/// The call's input (`Input`), after the `execute.before` hook dropped empty `model` and `sessionID`.
struct Input {
    agent: String,
    description: String,
    prompt: String,
    model: Option<String>,
    session_id: Option<String>,
    background: bool,
}

impl Input {
    fn decode(input: &Json) -> Result<Input, Failure> {
        let text = |key: &str| match json::get(input, key) {
            Some(Json::String(value)) => Ok(value.clone()),
            // TODO(port): the input schema's own error text.
            Some(_) => Err(failure(format!("Expected string at [\"{key}\"]"))),
            None => Err(failure(format!("Missing key at [\"{key}\"]"))),
        };
        let optional = |key: &str| match json::get(input, key) {
            None => Ok(None),
            Some(Json::String(value)) if value.is_empty() => Ok(None),
            Some(Json::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(failure(format!("Expected string at [\"{key}\"]"))),
        };
        Ok(Input {
            agent: text("agent")?,
            description: text("description")?,
            prompt: text("prompt")?,
            model: optional("model")?,
            session_id: optional("sessionID")?,
            background: match json::get(input, "background") {
                None => false,
                Some(Json::Bool(value)) => *value,
                Some(_) => return Err(failure("Expected boolean at [\"background\"]")),
            },
        })
    }
}

/// `resolveModel`: a `providerID/modelID[#variant]` reference to an available chat model (and one of its variants).
fn resolve_model(inventory: &Inventory, input: &str) -> Result<Json, Failure> {
    let invalid = || failure(format!("Invalid model \"{input}\". Use \"providerID/modelID\" or \"providerID/modelID#variant\"."));
    // `Model.Ref.parse`.
    let provider_end = input.find('/').filter(|&end| end > 0).ok_or_else(invalid)?;
    let provider = &input[..provider_end];
    let rest = &input[provider_end + 1..];
    let (id, variant) = match rest.find('#') {
        Some(index) => (&rest[..index], Some(&rest[index + 1..])),
        None => (rest, None),
    };
    if id.is_empty() || provider.contains('#') || variant.is_some_and(|v| v.is_empty() || v.contains('#')) {
        return Err(invalid());
    }
    let Some(model) = inventory.available_models().find(|m| m.provider_id.0 == provider && m.id == id) else {
        fn kind(m: &t::ModelInfo) -> Option<&str> {
            match &m.r#type {
                Some(t::ModelType::ModelType0(t::ModelType0::Image)) => Some("image"),
                Some(t::ModelType::ModelType0(t::ModelType0::Decision)) => Some("decision"),
                Some(t::ModelType::String(other)) if other == "image" || other == "decision" => Some(other.as_str()),
                _ => None,
            }
        }
        let other = inventory.catalog.other.iter().find(|m| m.provider_id.0 == provider && m.id == id && kind(m).is_some());
        return Err(failure(match other {
            Some(other) => format!(
                "Model \"{provider}/{id}\" is {} model; subagents need a chat model. Use the models tool to find one.",
                if kind(other) == Some("image") { "an image" } else { "a decision" }
            ),
            None => format!("Model \"{provider}/{id}\" is not available. Use the models tool to see what is available."),
        }));
    };
    if let Some(variant) = variant {
        if !model.variants.iter().any(|v| v.id == variant) {
            return Err(failure(if model.variants.is_empty() {
                format!("Model \"{provider}/{id}\" has no variants. Omit the variant.")
            } else {
                let available: Vec<&str> = model.variants.iter().map(|v| v.id.as_str()).collect();
                format!("Variant \"{variant}\" is not available for \"{provider}/{id}\". Available: {}.", available.join(", "))
            }));
        }
    }
    let mut reference = Obj::new();
    reference.str("id", id).str("providerID", provider);
    reference.opt_str("variant", variant);
    Ok(reference.done())
}

/// `permission.assert` for the agent, mapped as the tool's `ToolFailure` wraps the permission error: a rule's denial
/// or a rejection with feedback is shown as is, a plain rejection as `Subagent denied: <agent>`.
async fn assert_allowed(context: &Context, agent: &str) -> Result<(), Failure> {
    use crate::permission::{Ask, Error, Requester, Source};
    let requester = Requester {
        id: None,
        session_id: context.session_id.clone(),
        agent: Some(context.agent.clone()),
        source: Some(Source { message_id: context.message_id.clone(), id: context.call_id.clone() }),
    };
    let ask = Ask { action: NAME.into(), resources: vec![agent.into()], save: Some(vec![agent.into()]), metadata: None };
    match context.location.permission.assert(&context.app, &context.location, &requester, &ask).await {
        Ok(()) => Ok(()),
        Err(error @ Error::Blocked { .. }) => Err(Failure::Error(ToolError { kind: "permission.rejected", message: error.message(), metadata: None })),
        Err(Error::Corrected(feedback)) if !feedback.is_empty() => {
            Err(Failure::Error(ToolError { kind: "permission.rejected", message: feedback, metadata: None }))
        }
        Err(_) => Err(failure(format!("Subagent denied: {agent}"))),
    }
}

/// Interrupts the child and cancels its job when the foreground call is dropped (the parent was interrupted).
struct Orphaned {
    app: Arc<crate::app::App>,
    child: String,
    armed: bool,
}

impl Drop for Orphaned {
    fn drop(&mut self) {
        if self.armed {
            self.app.sessions.executions.interrupt(&self.child, "user");
            self.app.sessions.jobs.cancel(&self.app, &self.child);
        }
    }
}

fn running(child: &str) -> Output {
    let text = [
        format!("The subagent is working in the background (sessionID: {child}). You will be notified automatically when it finishes."),
        "DO NOT sleep, poll for progress, ask the subagent for status, or duplicate this subagent's work; avoid working with the same files or topics it is using.".to_owned(),
        "Work on non-overlapping tasks, or briefly tell the user what you launched and end your response.".to_owned(),
        "To send it follow-ups or change its instructions while it runs, call subagent again with this sessionID.".to_owned(),
    ]
    .join("\n");
    output(child, "running", text)
}

fn output(child: &str, status: &str, text: String) -> Output {
    let mut content = Obj::new();
    content.str("type", "text").str("text", &text);
    let mut metadata = Obj::new();
    metadata.str("sessionID", child).str("status", status);
    Output { content: vec![content.done()], metadata: Some(metadata.done()) }
}

async fn run(context: Context, input: Json) -> Result<Output, Failure> {
    let input = Input::decode(&input)?;
    let app = context.app.clone();
    let lookup = |id: &str| session::get(&app.db.lock().unwrap(), id).ok().flatten();
    let parent = lookup(&context.session_id).ok_or_else(|| failure(format!("Parent session not found: {}", context.session_id)))?;
    let mut depth = 0;
    let mut current = parent.clone();
    while let Some(parent_id) = json::get_str(&current.info, "parentID").map(str::to_owned) {
        depth += 1;
        current = lookup(&parent_id).ok_or_else(|| failure(format!("Parent session not found: {parent_id}")))?;
    }
    let inventory = context.location.inventory();
    let limit = inventory.latest(|c| c.experimental.as_ref()).and_then(|e| e.subagent_depth).unwrap_or(1);
    if depth >= limit {
        return Err(failure(format!(
            "Subagent depth limit reached ({limit}). Increase \"experimental.subagent_depth\" to allow nested subagents."
        )));
    }
    let Some(agent) = inventory.agent(&input.agent) else { return Err(failure(format!("Unknown agent: {}", input.agent))) };
    if matches!(agent.mode, t::AgentInfoMode::Primary) {
        return Err(failure(format!("Agent {} cannot run as a subagent", input.agent)));
    }
    assert_allowed(&context, &agent.id.0).await?;
    let existing = match &input.session_id {
        None => None,
        Some(id) => Some(lookup(id).ok_or_else(|| failure(format!("Subagent session not found: {id}")))?),
    };
    if let Some(existing) = &existing {
        if json::get_str(&existing.info, "parentID") != Some(context.session_id.as_str()) {
            return Err(failure(format!("Session {} is not a child of the current session", existing.id())));
        }
    }
    let requested = match &input.model {
        Some(model) => Some(resolve_model(&inventory, model)?),
        None => None,
    };
    let agent_model = agent.model.as_ref().map(json::of);
    // Continuing with another agent switches the child: an explicit model wins over the agent's, which wins over the
    // child's own.
    if let Some(existing) = &existing {
        let switched = json::get_str(&existing.info, "agent") != Some(agent.id.0.as_str());
        let model = requested.clone().or_else(|| if switched { agent_model.clone() } else { None });
        let switch_failed = |_| failure(format!("Failed to switch subagent session: {}", existing.id()));
        if switched {
            session::prompt::switch_agent(&app, existing.id(), &agent.id.0, Some(existing.location.clone()))
                .map_err(switch_failed)?;
        }
        if let Some(model) = model {
            session::prompt::switch_model(&app, existing.id(), model, Some(existing.location.clone())).map_err(switch_failed)?;
        }
    }
    let child = match &existing {
        Some(existing) => existing.id().to_owned(),
        None => {
            let model = requested.or(agent_model).or_else(|| json::get(&parent.info, "model").cloned());
            let created = session::create(
                &app,
                CreateInput {
                    id: None,
                    parent_id: Some(context.session_id.clone()),
                    title: Some(input.description.clone()),
                    agent: Some(agent.id.0.clone()),
                    model,
                    location: None,
                    metadata: None,
                    permissions: None,
                },
            )
            .map_err(|_| failure(format!("Parent session not found: {}", context.session_id)))?;
            created.id().to_owned()
        }
    };
    let mut progress = Obj::new();
    progress.str("sessionID", &child).str("status", "running");
    (context.progress)(progress.done());
    let mut prompt = Obj::new();
    match existing {
        Some(_) => prompt.str("text", &input.prompt),
        None => prompt.str("text", &format!("{PREAMBLE}\n{}", input.prompt)),
    };
    if input.background && existing.is_none() {
        prompt.set("resume", Json::Bool(false));
    }
    session::prompt::submit(&app, &child, &prompt.done())
        .await
        .map_err(|_| failure(format!("Failed to prompt subagent: {child}")))?;
    let spawned = Spawned { parent: context.session_id.clone(), child: child.clone(), agent: agent.name.clone(), description: input.description };
    jobs::start(&app, &spawned);
    if input.background {
        jobs::background(&app, &spawned);
        return Ok(running(&child));
    }
    let mut orphaned = Orphaned { app: app.clone(), child: child.clone(), armed: true };
    let blocked = app.sessions.jobs.block(&child, &context.session_id).await;
    orphaned.armed = false;
    let info = match blocked {
        Some(Blocked::Backgrounded(info)) => {
            jobs::notify(&app, &spawned, info.started_at);
            return Ok(running(&child));
        }
        Some(Blocked::Finished(info)) => Some(info),
        None => None,
    };
    // Failures keep the sessionID visible so the model can continue the child.
    match info.as_ref().map(|info| info.status) {
        Some(Status::Error) => Err(failure(format!(
            "Subagent failed (sessionID: {child}): {}",
            info.as_ref().and_then(|i| i.error.as_deref()).unwrap_or("unknown error")
        ))),
        Some(Status::Cancelled) => Err(failure(format!("Subagent cancelled (sessionID: {child})"))),
        _ => {
            let text = info.and_then(|i| i.output).unwrap_or_else(|| NO_TEXT.to_owned());
            Ok(output(&child, "completed", format!("<subagent sessionID=\"{child}\" state=\"completed\">\n{text}\n</subagent>")))
        }
    }
}

impl Tool for Subagent {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(run(context, input))
    }
}
