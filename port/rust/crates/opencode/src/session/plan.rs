//! Plan mode (packages/core/src/plugin/plan.ts): a session that enters the `plan` agent gets a reminder of Plan mode's
//! rules as synthetic input (on creation, and on a switch to or from the agent), and every request reconciles the
//! reminder near the tail of the history so compaction or a revert cannot strip it.
//!
//! TODO(port): the agent switch (`session.agent.selected`) and the plan agent's edit/write permission message
//! rewrite (`tool.execute.after`).

use crate::app::App;
use crate::bus::LocationRef;
use crate::json::Obj;
use crate::llm::{Message, UserPart};

pub const AGENT: &str = "plan";

fn directory() -> String {
    let home = std::env::var("OPENCODE_TEST_HOME").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    format!("{home}/.opencode/plan")
}

pub fn enter() -> String {
    format!(
        "<system-reminder>
You are in Plan mode. Discuss the plan with the user directly in the conversation. Do not create or update plan files unless the user explicitly asks you to; when they do, write them only in:
{}

Do not modify any other files or ask a subagent to do so.

You remain in Plan mode until the user switches agents. If the user asks you to implement changes, do not do so. Tell them they need to switch agents.
</system-reminder>",
        directory()
    )
}

const LEAVE: &str = "<system-reminder>
You are NO LONGER in Plan mode. The previous Plan restrictions no longer apply. Any Plan mode instructions from earlier in this conversation are no longer active.
</system-reminder>";

/// `session.created` with the plan agent: the reminder, admitted without waking the session.
pub fn created(app: &App, session: &str, agent: Option<&str>, location: &LocationRef) {
    if agent != Some(AGENT) {
        return;
    }
    let mut payload = Obj::new();
    payload.str("text", &enter());
    let mut item = Obj::new();
    item.str("type", "synthetic").set("payload", payload.done()).str("delivery", "steer");
    let id = crate::session::message::create_id();
    if let Err(error) = crate::session::inbox::admit(app, &id, session, item.done(), Some(location.clone())) {
        eprintln!("failed to inject Plan mode reminder: {}", crate::json::text(&error.body));
    }
}

/// The `context` hook: the reminder the agent needs, before the user's prompt.
pub fn reconcile(messages: &mut Vec<Message>, agent: &str) {
    let enter = enter();
    let last = messages.iter().fold(None, |found, message| match message {
        Message::User(parts) if parts.len() == 1 => match &parts[0] {
            UserPart::Text(text) if *text == enter || text == LEAVE => Some(text.clone()),
            _ => found,
        },
        _ => found,
    });
    let text = if agent == AGENT && last.as_deref() != Some(enter.as_str()) {
        Some(enter.clone())
    } else if agent != AGENT && last.as_deref() == Some(enter.as_str()) {
        Some(LEAVE.to_owned())
    } else {
        None
    };
    let Some(text) = text else { return };
    let at = if matches!(messages.last(), Some(Message::User(_))) { messages.len() - 1 } else { messages.len() };
    messages.insert(at, Message::User(vec![UserPart::Text(text)]));
}
