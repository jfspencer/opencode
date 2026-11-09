//! Title generation (packages/core/src/session/title.ts, context.ts `selectTitle`): an untitled session's first input
//! starts it beside the turn; `PATCH /session/:id {title: ""}` regenerates it from the conversation. The title agent's
//! model (or the session's) is asked with the agent's prompt; the first non-empty line of its reply, without
//! `<think>` blocks, becomes the title (`session.renamed`) unless the session was renamed meanwhile. The request's
//! usage is recorded as `session.usage.recorded` (an internal durable event).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use opencode_protocol::Json;
use tokio::sync::mpsc;

use crate::app::App;
use crate::json::{self, Obj};
use crate::llm::{self, Event};
use crate::session::{context, message, step};

const MAX_LENGTH: usize = 100;
const MAX_CONTEXT_LENGTH: usize = 8_000;
const MAX_FIRST_MESSAGE_LENGTH: usize = 2_000;

fn truncate(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    if units.len() <= MAX_LENGTH {
        return value.to_owned();
    }
    format!("{}...", String::from_utf16_lossy(&units[..MAX_LENGTH - 3]))
}

fn message_prompt(text: &str) -> String {
    format!("Here is the first message of the conversation:\n\n<message>\n{text}\n</message>\n\nReply with only the title.")
}

fn conversation_prompt(text: &str) -> String {
    format!("Here is the conversation so far:\n\n<conversation>\n{text}\n</conversation>\n\nReply with only the title.")
}

/// `new Date(ms).toISOString()`.
fn iso(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rest = ms.rem_euclid(86_400_000);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

/// `SessionTitle.isUntitled`: no title, or the exact root fallback (`New session - <created ISO>`).
pub fn untitled(session: &Json) -> bool {
    match json::get_str(session, "title") {
        None => true,
        Some(title) => {
            let created = json::get(session, "time").and_then(|t| json::get_num(t, "created")).unwrap_or(0.0) as i64;
            title == format!("New session - {}", iso(created))
        }
    }
}

fn running() -> &'static Mutex<HashSet<String>> {
    static RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

/// Starts generation in the background unless one is running for the session (`FiberMap.run`, `onlyIfMissing`).
pub fn spawn(app: &Arc<App>, session: &str) {
    if !running().lock().unwrap().insert(session.to_owned()) {
        return;
    }
    let app = app.clone();
    let session = session.to_owned();
    tokio::spawn(async move {
        generate(&app, &session).await;
        running().lock().unwrap().remove(&session);
    });
}

/// One title request (`attempt`): the title, or `None` when the request failed or said nothing.
async fn attempt(app: &App, session: &Json, agent: &Json, model: Arc<llm::Model>, text: &str) -> Option<String> {
    let system: Vec<String> = json::get_str(agent, "system").map(|s| vec![s.to_owned()]).unwrap_or_default();
    let request = llm::Request {
        model: model.clone(),
        system,
        messages: vec![llm::Message::User(vec![llm::UserPart::Text(text.to_owned())])],
        tools: vec![],
        tool_choice: None,
        max_tokens: None,
        prompt_cache_key: Some(context::cache_key(session)),
        headers: context::headers(session),
    };
    let (sender, mut receiver) = mpsc::channel(64);
    let streaming = tokio::spawn(async move { llm::stream(&request, sender).await });
    let mut chunks = String::new();
    let mut usage: Option<(f64, Json)> = None;
    let mut failed = false;
    while let Some(next) = receiver.recv().await {
        match next {
            Ok(Event::TextDelta { text, .. }) => chunks.push_str(&text),
            Ok(Event::StepFinish { usage: step_usage, .. }) => {
                let (cost, tokens) = step::usage(step_usage.as_ref(), &model.cost);
                let cost = match cost {
                    Json::Number(n) => n,
                    _ => 0.0,
                };
                usage = Some(match usage {
                    None => (cost, tokens),
                    Some((total, previous)) => (total + cost, add_tokens(&previous, &tokens)),
                });
            }
            Ok(_) => {}
            Err(_) => failed = true,
        }
    }
    let _ = streaming.await;
    let session_id = json::get_str(session, "id").unwrap_or_default();
    if let Some((cost, tokens)) = usage {
        let mut data = Obj::new();
        data.str("sessionID", session_id).str("source", "title").set("cost", Json::Number(cost)).set("tokens", tokens);
        let location = crate::session::require(&app.db.lock().unwrap(), session_id).ok().map(|s| s.location);
        let _ = crate::session::publish_event(app, "session.usage.recorded", data.done(), location, None);
    }
    if failed {
        return None;
    }
    let without_think = strip_think(&chunks);
    without_think.split('\n').map(str::trim).find(|line| !line.is_empty()).map(str::to_owned)
}

/// `replace(/<think>[\s\S]*?<\/think>\s*/g, "")`.
fn strip_think(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        let Some(end) = rest[start..].find("</think>") else { break };
        out.push_str(&rest[..start]);
        rest = rest[start + end + "</think>".len()..].trim_start_matches(|c: char| c.is_whitespace());
    }
    out.push_str(rest);
    out
}

fn add_tokens(a: &Json, b: &Json) -> Json {
    let n = |v: &Json, k: &str| json::get_num(v, k).unwrap_or(0.0);
    let cache = |v: &Json, k: &str| json::get(v, "cache").and_then(|c| json::get_num(c, k)).unwrap_or(0.0);
    let mut c = Obj::new();
    c.set("read", Json::Number(cache(a, "read") + cache(b, "read"))).set("write", Json::Number(cache(a, "write") + cache(b, "write")));
    let mut o = Obj::new();
    o.set("input", Json::Number(n(a, "input") + n(b, "input")))
        .set("output", Json::Number(n(a, "output") + n(b, "output")))
        .set("reasoning", Json::Number(n(a, "reasoning") + n(b, "reasoning")))
        .set("cache", c.done());
    o.done()
}

/// Variant IDs that minimize reasoning output, in preference order.
const MINIMAL: &[&str] = &["none", "minimal", "low"];

fn variants(info: &Json) -> Vec<String> {
    match json::get(info, "variants") {
        Some(Json::Array(v)) => v.iter().filter_map(|v| json::get_str(v, "id").map(str::to_owned)).collect(),
        _ => vec![],
    }
}

/// `minimalVariant`: the first of [`MINIMAL`] the model has.
fn minimal(info: &Json) -> Option<String> {
    let variants = variants(info);
    MINIMAL.iter().find(|m| variants.iter().any(|v| v == *m)).map(|m| (*m).to_owned())
}

/// `lowerVariant`: `none` and `minimal` always reduce reasoning; `low` can enable reasoning on a model whose default
/// does not reason, so it is only used when it ranks below the selected variant.
fn lower(info: &Json, selected: Option<&str>) -> Option<String> {
    let variants = variants(info);
    let rank = |id: Option<&str>| variants.iter().position(|v| Some(v.as_str()) == id).map_or(-1, |i| i as i64);
    MINIMAL.iter().find(|id| rank(Some(id)) != -1 && (**id != "low" || rank(Some(id)) < rank(selected))).map(|id| (*id).to_owned())
}

/// Families of small models, in preference order (`Model.small`).
const SMALL_FAMILIES: &[&str] = &["gpt-luna", "gemini-flash-lite", "gemini-flash", "claude-haiku"];

/// `Model.small`: the provider's available, active text model of the most preferred small family.
fn small(inventory: &crate::inventory::Inventory, provider: &str) -> Option<Json> {
    let text = |info: &Json, key: &str| match json::get(info, "capabilities").and_then(|c| json::get(c, key)) {
        Some(Json::Array(items)) => items.iter().filter_map(json::as_str).any(|item| item.starts_with("text")),
        _ => false,
    };
    let models: Vec<Json> = inventory
        .available_models()
        .filter(|m| m.provider_id.0 == provider)
        .map(json::of)
        .filter(|m| json::get_str(m, "status") == Some("active") && text(m, "input") && text(m, "output"))
        .collect();
    SMALL_FAMILIES.iter().find_map(|family| models.iter().find(|m| json::get_str(m, "family") == Some(*family)).cloned())
}

/// `SessionTitle.generate`.
pub async fn generate(app: &Arc<App>, session_id: &str) {
    let Some(stored) = crate::session::get(&app.db.lock().unwrap(), session_id).ok().flatten() else { return };
    let session = stored.info.clone();
    let Some((first_id, first_text)) = message::first_user(&app.db.lock().unwrap(), session_id).ok().flatten() else { return };
    let text = if untitled(&session) {
        message_prompt(&first_text)
    } else {
        let history = message::history(&app.db.lock().unwrap(), session_id).unwrap_or_default();
        let original = format!("Original request:\n{}", first_text.chars().take(MAX_FIRST_MESSAGE_LENGTH).collect::<String>());
        let recent: Vec<String> = history
            .iter()
            .filter_map(|m| match json::get_str(m, "type") {
                Some("user") if json::get_str(m, "id") != Some(first_id.as_str()) => {
                    Some(format!("User: {}", json::get_str(m, "text").unwrap_or_default().trim()))
                }
                Some("assistant") => {
                    let parts: Vec<String> = match json::get(m, "content") {
                        Some(Json::Array(content)) => content
                            .iter()
                            .filter(|p| json::get_str(p, "type") == Some("text"))
                            .filter_map(|p| json::get_str(p, "text").map(|t| t.trim().to_owned()))
                            .filter(|t| !t.is_empty())
                            .collect(),
                        _ => vec![],
                    };
                    (!parts.is_empty()).then(|| format!("Assistant: {}", parts.join("\n")))
                }
                _ => None,
            })
            .collect();
        let recent = recent.join("\n\n");
        if recent.is_empty() {
            conversation_prompt(&original)
        } else {
            let prefix = format!("{original}\n\nRecent conversation:\n");
            let budget = MAX_CONTEXT_LENGTH as i64 - conversation_prompt("").encode_utf16().count() as i64 - prefix.encode_utf16().count() as i64;
            let units: Vec<u16> = recent.encode_utf16().collect();
            // `recent.slice(-budget)`.
            let start = if budget > 0 { units.len().saturating_sub(budget as usize) } else { ((-budget) as usize).min(units.len()) };
            let tail = String::from_utf16_lossy(&units[start..]);
            conversation_prompt(&format!("{prefix}{tail}"))
        }
    };
    let Ok(location) = app.locations.get(app, &stored.location).await else { return };
    let inventory = location.inventory();
    let inventory = &*inventory;
    // `selectTitle`.
    let Some(agent) = context::agent(inventory, "title") else { return };
    let primary = context::select_agent(inventory, &session)
        .ok()
        .and_then(|(_, a)| context::resolve_model(inventory, &session, Some(&a)).ok());
    let candidate = match json::get(&agent, "model") {
        Some(reference) => {
            let provider = json::get_str(reference, "providerID").unwrap_or_default();
            let id = json::get_str(reference, "id").unwrap_or_default();
            inventory.model(provider, id).map(json::of).map(|info| {
                let variant = json::get_str(reference, "variant").map(str::to_owned).or_else(|| minimal(&info));
                (info, variant)
            })
        }
        None => primary.as_ref().and_then(|primary| {
            let provider = json::get_str(&primary.reference, "providerID").unwrap_or_default();
            if let Some(small) = small(inventory, provider) {
                let variant = minimal(&small);
                return Some((small, variant));
            }
            // Without a lower variant, the primary model at its selected variant is already the fallback.
            let info = inventory.model(provider, json::get_str(&primary.reference, "id").unwrap_or_default()).map(json::of)?;
            let variant = lower(&info, json::get_str(&primary.reference, "variant"))?;
            Some((info, Some(variant)))
        }),
    };
    let preferred = candidate.and_then(|(info, variant)| {
        let mut reference = Obj::new();
        reference.str("providerID", json::get_str(&info, "providerID").unwrap_or_default());
        reference.str("id", json::get_str(&info, "id").unwrap_or_default());
        reference.opt_str("variant", variant.as_deref());
        let mut with_model = session.clone();
        json::set(&mut with_model, "model", reference.done());
        context::resolve_model(inventory, &with_model, None).ok()
    });
    let Some(selected) = preferred.clone().or_else(|| primary.clone()) else { return };
    let mut title = attempt(app, &session, &agent, selected.clone(), &text).await;
    if title.is_none() {
        if let Some(primary) = primary.filter(|p| p.reference != selected.reference) {
            title = attempt(app, &session, &agent, primary, &text).await;
        }
    }
    let Some(title) = title else { return };
    let title = truncate(&title);
    let expected = crate::bus::latest_sequence(&app.db.lock().unwrap(), session_id).unwrap_or(-1) + 1;
    let Some(current) = crate::session::get(&app.db.lock().unwrap(), session_id).ok().flatten() else { return };
    let before = json::get_str(&session, "title");
    let now = json::get_str(&current.info, "title");
    if now != before || now == Some(title.as_str()) {
        return;
    }
    let mut data = Obj::new();
    data.str("sessionID", session_id).str("title", &title);
    // The rename applies only if nothing else was recorded since the title was read.
    let _ = crate::session::publish_committed(app, "session.renamed", data.done(), Some(stored.location.clone()), None, |_, event| {
        match event.durable.as_ref().map(|d| d.seq) {
            Some(seq) if seq == expected => Ok(()),
            _ => Err(rusqlite::Error::QueryReturnedNoRows),
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn iso_dates() {
        assert_eq!(super::iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::iso(1_767_225_600_123), "2026-01-01T00:00:00.123Z");
        assert_eq!(super::strip_think("<think>a</think>\n  Title"), "Title");
    }
}
