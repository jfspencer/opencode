//! `routes/session/rows.ts`: the transcript's rows (messages, parts, groups, footers, usage), rebuilt from the
//! store on sync and on structural changes, and appended to on live events in between.
//!
//! The rows are one signal of plain values; `<For>` keys them by [`row_key`] (TS keys its store reconcile the
//! same way), so a row whose key stays keeps its component.
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::*;
use opencode_tui_sync::bus::Unsubscribe;
use opencode_tui_sync::connection::Status;
use opencode_tui_sync::data::LoadMoreOptions;
use opencode_tui_sync::event::Event;
use opencode_tui_sync::json::JsonExt;

use super::anchors::group_id;
use super::grouping::*;
use crate::ctx::tui;

/// The rows of one session.
pub type Rows = Signal<Vec<SessionRow>>;

fn str_of<'a>(value: &'a Json, key: &str) -> &'a str {
    value.str(key).unwrap_or_default()
}

fn message_id(message: &Json) -> &str {
    str_of(message, "id")
}

fn created(message: &Json) -> Option<f64> {
    message.get("time").and_then(|t| t.num("created"))
}

/// `completeGroupBoundary(input)`: while the oldest row is a group, keep loading older pages until something
/// precedes it.
pub async fn complete_group_boundary(
    rows: Rows,
    messages: impl Fn() -> usize,
    more: impl Fn() -> bool,
    load_more: impl Fn() -> opencode_tui_sync::data::Promise<()>,
    active: impl Fn() -> bool,
) -> Result<(), ()> {
    while active() && matches!(rows.get_untracked().first(), Some(SessionRow::Group(_))) && more() {
        let before = messages();
        load_more().wait().await.map_err(|_| ())?;
        // A page that adds nothing would otherwise loop forever.
        if messages() == before {
            return Ok(());
        }
    }
    Ok(())
}

/// `createSessionRows(sessionID, onSynced, beforePublish)`.
pub fn create_session_rows(
    session_id: Rc<dyn Fn() -> String>,
    on_synced: Rc<dyn Fn(&str)>,
    before_publish: Rc<dyn Fn()>,
) -> Rows {
    let t = tui();
    let data = t.data.clone();
    let config = t.config.clone();
    let rows: Rows = create_signal_with(Vec::new(), None).0;
    let revert_boundary = {
        let (data, session_id) = (data.clone(), session_id.clone());
        move || data.store.session.get(&session_id()).and_then(|s| s.get("revert").and_then(|r| r.str("messageID")).map(str::to_string))
    };
    let turn_tokens = {
        let config = config.clone();
        move || config.debug.as_ref().and_then(|d| d.turn_tokens.as_ref()).is_some_and(truthy)
    };
    let verbosity = {
        let config = config.clone();
        move || Verbosity::parse(config.session.verbosity.as_deref())
    };

    let pending_permissions = {
        let (data, session_id) = (data.clone(), session_id.clone());
        Rc::new(move || -> HashSet<String> {
            data.store
                .session
                .permission
                .list(&session_id())
                .map(|list| {
                    list.iter()
                        .filter(|r| r.get("source").and_then(|s| s.str("type")) == Some("tool"))
                        .filter_map(|r| r.get("source").and_then(|s| s.str("id")).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        })
    };

    let reduce = {
        let (data, session_id) = (data.clone(), session_id.clone());
        let pending_permissions = pending_permissions.clone();
        let (revert_boundary, turn_tokens, verbosity) = (revert_boundary.clone(), turn_tokens.clone(), verbosity.clone());
        Rc::new(move || -> Vec<SessionRow> {
            let id = session_id();
            let messages = data.store.session.message.list(&id);
            let inputs: HashSet<String> = data.store.session.input.list(&id).into_iter().collect();
            let pending = data.store.session.pending.list(&id);
            let queued: HashSet<&str> = pending
                .iter()
                .filter(|item| item.str("type") == Some("user") && item.str("delivery") == Some("queue"))
                .map(|item| str_of(item, "id"))
                .collect();
            let boundary = revert_boundary();
            let visible: Vec<Rc<Json>> = messages
                .iter()
                .filter(|m| queued.is_empty() || !queued.contains(message_id(m)))
                .filter(|m| boundary.as_deref().is_none_or(|b| message_id(m) < b))
                .cloned()
                .collect();
            let mut rows = reduce_session_rows(&visible, &inputs, turn_tokens(), verbosity());
            partition_pending(&mut rows, &pending_permissions());
            let position = rows
                .iter()
                .position(|row| matches!(row, SessionRow::Entry(SessionEntry::Message { message_id }) if inputs.contains(message_id)))
                .unwrap_or(rows.len());
            let compactions: Vec<SessionRow> = pending
                .iter()
                .filter(|item| item.str("type") == Some("compaction"))
                .map(|item| SessionRow::Entry(SessionEntry::CompactionQueued { inbox_id: str_of(item, "id").to_string() }))
                .collect();
            rows.splice(position..position, compactions);
            rows
        })
    };

    let rebuild: Rc<dyn Fn()> = {
        let reduce = reduce.clone();
        Rc::new(move || {
            let next = untrack(|| reduce());
            rows.set(next);
        })
    };

    {
        let pending_permissions = pending_permissions.clone();
        create_effect(move || {
            let pending = pending_permissions();
            rows.update(|draft| partition_pending(draft, &pending));
        });
    }

    {
        let (data, session_id) = (data.clone(), session_id.clone());
        let rebuild = rebuild.clone();
        let on_synced = on_synced.clone();
        let before_publish = before_publish.clone();
        let connection = data.connection.clone();
        let sid = session_id.clone();
        create_effect(on(
            move || (sid(), connection.status()),
            move |(id, status), _| {
                if *status != Status::Connected {
                    return;
                }
                rebuild();
                let id = id.clone();
                let pending = data.store.session.pending.sync(&id);
                tokio::task::spawn_local(async move {
                    let _ = pending.wait().await;
                });
                let sync = data.store.session.message.sync(&id);
                let (data, session_id, rebuild, on_synced, before_publish) =
                    (data.clone(), session_id.clone(), rebuild.clone(), on_synced.clone(), before_publish.clone());
                tokio::task::spawn_local(async move {
                    if sync.wait().await.is_err() {
                        return;
                    }
                    if session_id() != id {
                        return;
                    }
                    rebuild();
                    // Restoration waits for complete boundary groups so saved group IDs resolve.
                    let (d1, d2, d3) = (data.clone(), data.clone(), data.clone());
                    let (i1, i2, i3) = (id.clone(), id.clone(), id.clone());
                    let s = session_id.clone();
                    let before = before_publish.clone();
                    let _ = complete_group_boundary(
                        rows,
                        move || d1.store.session.message.list(&i1).len(),
                        move || d2.store.session.message.more(&i2),
                        move || {
                            let before = before.clone();
                            d3.store.session.message.load_more(&i3, LoadMoreOptions { before_publish: Some(Rc::new(move || before())), ..LoadMoreOptions::default() })
                        },
                        { let id = id.clone(); move || s() == id },
                    )
                    .await;
                    let id = id.clone();
                    if session_id() == id {
                        on_synced(&id);
                    }
                });
            },
            false,
        ));
    }

    // Re-reduce when the revert boundary changes (stage/clear/commit). These reactions defer their first run: the
    // mount effect above has already reduced the same state.
    {
        let rebuild = rebuild.clone();
        create_effect(on(revert_boundary.clone(), move |_, _| rebuild(), true));
    }
    {
        let (data, session_id) = (data.clone(), session_id.clone());
        let rebuild = rebuild.clone();
        create_effect(on(
            move || {
                data.store
                    .session
                    .pending
                    .list(&session_id())
                    .iter()
                    .filter_map(|item| match (item.str("type"), item.str("delivery")) {
                        (Some("compaction"), _) => Some(format!("{}:compaction", str_of(item, "id"))),
                        (Some("user"), Some("queue")) => Some(format!("{}:queue", str_of(item, "id"))),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            },
            move |_, _| rebuild(),
            true,
        ));
    }
    {
        let (data, session_id) = (data.clone(), session_id.clone());
        let rebuild = rebuild.clone();
        create_effect(on(
            move || {
                let id = session_id();
                data.store
                    .session
                    .message
                    .list(&id)
                    .iter()
                    .filter_map(|message| match message.str("type") {
                        Some("user" | "synthetic") => Some((
                            message_id(message).to_string(),
                            created(message),
                            Some(data.store.session.input.has(&id, message_id(message))),
                        )),
                        Some("compaction" | "idle") => Some((message_id(message).to_string(), created(message), None)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            },
            move |_, _| rebuild(),
            true,
        ));
    }
    {
        let rebuild = rebuild.clone();
        let (turn_tokens, verbosity) = (turn_tokens.clone(), verbosity.clone());
        create_effect(on(move || (turn_tokens(), verbosity()), move |_, _| rebuild(), true));
    }

    let is_pending = {
        let (data, session_id) = (data.clone(), session_id.clone());
        Rc::new(move |message_id: &str| -> bool {
            let id = session_id();
            let message = data.store.session.message.get(&id, message_id);
            match message.as_ref().and_then(|m| m.str("type")) {
                Some("user" | "synthetic") => data.store.session.input.has(&id, message_id),
                Some("compaction") => message.as_ref().and_then(|m| m.str("status")) == Some("running"),
                _ => false,
            }
        })
    };
    let queued_start = {
        let is_pending = is_pending.clone();
        Rc::new(move |rows: &[SessionRow]| -> usize {
            rows.iter()
                .position(|row| match row {
                    SessionRow::Entry(SessionEntry::CompactionQueued { .. }) => true,
                    SessionRow::Entry(SessionEntry::Message { message_id }) => is_pending(message_id),
                    _ => false,
                })
                .unwrap_or(rows.len())
        })
    };

    let append_message: Rc<dyn Fn(&str)> = {
        let (data, session_id) = (data.clone(), session_id.clone());
        let (is_pending, queued_start) = (is_pending.clone(), queued_start.clone());
        Rc::new(move |message_id: &str| {
            let message_id = message_id.to_string();
            let (data, session_id, is_pending, queued_start) = (data.clone(), session_id.clone(), is_pending.clone(), queued_start.clone());
            untrack(move || {
                rows.update(|draft| {
                    if draft.iter().any(|row| matches!(row, SessionRow::Entry(SessionEntry::Message { message_id: m }) if *m == message_id)) {
                        return;
                    }
                    let pending = is_pending(&message_id);
                    let message = data.store.session.message.get(&session_id(), &message_id);
                    let compaction = message.as_ref().and_then(|m| m.str("type")) == Some("compaction");
                    let index = if compaction && pending {
                        queued_start(draft)
                    } else if pending {
                        draft.len()
                    } else {
                        queued_start(draft)
                    };
                    if !pending {
                        complete_previous(draft, index);
                    }
                    draft.insert(index, SessionRow::Entry(SessionEntry::Message { message_id: message_id.clone() }));
                })
            });
        })
    };

    let append_part: Rc<dyn Fn(PartRef, AppendPart)> = {
        let queued_start = queued_start.clone();
        let verbosity = verbosity.clone();
        Rc::new(move |r: PartRef, part: AppendPart| {
            let (queued_start, verbosity) = (queued_start.clone(), verbosity.clone());
            untrack(move || {
                rows.update(|draft| {
                    if !has_part(draft, &r) {
                        let index = queued_start(draft);
                        append(draft, r, &part, index, verbosity());
                        return;
                    }
                    let AppendPart::Reasoning { completed: Some(_) } = part else { return };
                    for row in draft.iter_mut() {
                        if let SessionRow::Group(group) = row {
                            if group.kind == GroupKind::Reasoning && group_refs(group, false).contains(&r) {
                                group.completed = true;
                                break;
                            }
                        }
                    }
                })
            });
        })
    };

    let append_footer: Rc<dyn Fn(&str)> = {
        let queued_start = queued_start.clone();
        Rc::new(move |message_id: &str| {
            let message_id = message_id.to_string();
            let queued_start = queued_start.clone();
            untrack(move || {
                rows.update(|draft| {
                    if draft.iter().any(|row| matches!(row, SessionRow::Entry(SessionEntry::AssistantFooter { message_id: m }) if *m == message_id)) {
                        return;
                    }
                    let index = queued_start(draft);
                    complete_previous(draft, index);
                    draft.insert(index, SessionRow::Entry(SessionEntry::AssistantFooter { message_id: message_id.clone() }));
                })
            });
        })
    };

    let remove_footer: Rc<dyn Fn(&str)> = Rc::new(move |message_id: &str| {
        let message_id = message_id.to_string();
        untrack(move || {
            rows.update(|draft| {
                if let Some(index) = draft.iter().position(|row| matches!(row, SessionRow::Entry(SessionEntry::AssistantFooter { message_id: m }) if *m == message_id)) {
                    draft.remove(index);
                }
            })
        });
    });

    let ours = {
        let session_id = session_id.clone();
        move |event: &Event| event.data().str("sessionID") == Some(session_id().as_str())
    };
    let from_event = |event: &Event| event.id().replacen("evt_", "msg_", 1);
    let mut subscriptions: Vec<Unsubscribe> = Vec::new();
    let store = &data.store;
    {
        let (ours, append_message) = (ours.clone(), append_message.clone());
        subscriptions.push(store.on("session.inbox.enqueued", move |event| {
            let item = event.data().get("item");
            let kind = item.and_then(|i| i.str("type"));
            let described = item.and_then(|i| i.get("payload")).and_then(|p| p.str("description")).is_some_and(|d| !d.trim().is_empty());
            if ours(event) && (kind == Some("user") || (kind == Some("synthetic") && described)) {
                append_message(str_of(event.data(), "inboxID"));
            }
        }));
    }
    {
        let (ours, append_message) = (ours.clone(), append_message.clone());
        subscriptions.push(store.on("session.compaction.started", move |event| {
            if ours(event) {
                let id = event.data().str("inputID").map(str::to_string).unwrap_or_else(|| from_event(event));
                append_message(&id);
            }
        }));
    }
    for kind in ["session.instructions.updated", "session.shell.started", "session.agent.selected", "session.model.selected"] {
        let (ours, append_message) = (ours.clone(), append_message.clone());
        subscriptions.push(store.on(kind, move |event| {
            if ours(event) {
                append_message(&from_event(event));
            }
        }));
    }
    {
        let (ours, append_message) = (ours.clone(), append_message.clone());
        subscriptions.push(store.on("session.synthetic", move |event| {
            if ours(event) && event.data().str("description").is_some_and(|d| !d.trim().is_empty()) {
                append_message(&from_event(event));
            }
        }));
    }
    let part_ref = |event: &Event, kind: &str| {
        let data = event.data();
        let ordinal = data.num("ordinal").unwrap_or(0.0);
        PartRef::new(str_of(data, "assistantMessageID"), &format!("{kind}:{ordinal}"))
    };
    for (kind, key, part) in [
        ("session.text.delta", "delta", "text"),
        ("session.text.ended", "text", "text"),
        ("session.reasoning.delta", "delta", "reasoning"),
    ] {
        let (ours, append_part) = (ours.clone(), append_part.clone());
        subscriptions.push(store.on(kind, move |event| {
            if ours(event) && !str_of(event.data(), key).trim().is_empty() {
                let appended = if part == "text" { AppendPart::Text } else { AppendPart::Reasoning { completed: None } };
                append_part(part_ref(event, part), appended);
            }
        }));
    }
    {
        let (ours, append_part) = (ours.clone(), append_part.clone());
        subscriptions.push(store.on("session.reasoning.ended", move |event| {
            if ours(event) && !str_of(event.data(), "text").trim().is_empty() {
                append_part(part_ref(event, "reasoning"), AppendPart::Reasoning { completed: event.created() });
            }
        }));
    }
    {
        let (ours, append_part) = (ours.clone(), append_part.clone());
        subscriptions.push(store.on("session.tool.input.started", move |event| {
            if ours(event) {
                let data = event.data();
                append_part(
                    PartRef::new(str_of(data, "assistantMessageID"), str_of(data, "id")),
                    AppendPart::Tool { name: str_of(data, "name").to_string() },
                );
            }
        }));
    }
    {
        let (ours, append_footer) = (ours.clone(), append_footer.clone());
        subscriptions.push(store.on("session.retry.scheduled", move |event| {
            if ours(event) {
                append_footer(str_of(event.data(), "assistantMessageID"));
            }
        }));
    }
    {
        let (ours, remove_footer) = (ours.clone(), remove_footer.clone());
        subscriptions.push(store.on("session.step.started", move |event| {
            if ours(event) {
                remove_footer(str_of(event.data(), "assistantMessageID"));
            }
        }));
    }
    {
        let (ours, append_footer, rebuild, turn_tokens) = (ours.clone(), append_footer.clone(), rebuild.clone(), turn_tokens.clone());
        subscriptions.push(store.on("session.step.ended", move |event| {
            let finish = str_of(event.data(), "finish");
            if !ours(event) || finish == "tool-calls" || finish == "unknown" {
                return;
            }
            append_footer(str_of(event.data(), "assistantMessageID"));
            if turn_tokens() {
                rebuild();
            }
        }));
    }
    {
        let (ours, append_footer, rebuild, turn_tokens) = (ours.clone(), append_footer.clone(), rebuild.clone(), turn_tokens.clone());
        subscriptions.push(store.on("session.step.failed", move |event| {
            if !ours(event) {
                return;
            }
            append_footer(str_of(event.data(), "assistantMessageID"));
            if turn_tokens() {
                rebuild();
            }
        }));
    }
    let subscriptions = RefCell::new(subscriptions);
    on_cleanup(move || subscriptions.take().into_iter().for_each(Unsubscribe::call));
    rows
}

fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => *n != 0.0 && !n.is_nan(),
        Json::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn json_string(text: &str) -> String {
    opencode_tui_sync::client::stringify(&Json::String(text.to_string()))
}

/// `rowKey(row)`.
pub fn row_key(row: &SessionRow) -> String {
    match row {
        SessionRow::Group(group) => group_id(&group.node(), 0).unwrap_or_default(),
        SessionRow::Entry(SessionEntry::Part { r#ref }) => {
            format!("[\"part\",{},{}]", json_string(&r#ref.message_id), json_string(&r#ref.part_id))
        }
        SessionRow::Entry(SessionEntry::Message { message_id }) => format!("[\"message\",{}]", json_string(message_id)),
        SessionRow::Entry(SessionEntry::AssistantFooter { message_id }) => {
            format!("[\"assistant-footer\",{}]", json_string(message_id))
        }
        SessionRow::Entry(SessionEntry::CompactionQueued { inbox_id }) => {
            format!("[\"compaction-queued\",{}]", json_string(inbox_id))
        }
        SessionRow::Entry(SessionEntry::TurnUsage { message_ids, .. }) => format!(
            "[\"turn-usage\",{}]",
            message_ids.first().map(|id| json_string(id)).unwrap_or_else(|| "null".into())
        ),
    }
}

fn token(tokens: &Json, key: &str) -> f64 {
    tokens.num(key).unwrap_or(0.0)
}

fn token_total(tokens: &Json) -> f64 {
    let cache = tokens.get("cache");
    token(tokens, "input")
        + token(tokens, "output")
        + token(tokens, "reasoning")
        + cache.map_or(0.0, |c| token(c, "read"))
        + cache.map_or(0.0, |c| token(c, "write"))
}

fn has_token_usage(message: &Json) -> bool {
    message.get("tokens").is_some_and(|t| token_total(t) > 0.0)
}

/// The error identity TS compares (`type` and `message`).
fn error_identity(error: Option<&Json>) -> Option<(String, String)> {
    error.map(|e| (str_of(e, "type").to_string(), str_of(e, "message").to_string()))
}

/// `reduceSessionRows(messages, inputs, turnTokens, verbosity)`.
pub fn reduce_session_rows(messages: &[Rc<Json>], inputs: &HashSet<String>, turn_tokens: bool, verbosity: Verbosity) -> Vec<SessionRow> {
    let is_input = |m: &Json| inputs.contains(message_id(m));
    let pending_compactions: Vec<&Rc<Json>> =
        messages.iter().filter(|m| m.str("type") == Some("compaction") && m.str("status") == Some("running")).collect();
    let pending: HashSet<&str> =
        pending_compactions.iter().map(|m| message_id(m)).chain(inputs.iter().map(String::as_str)).collect();
    struct Usage {
        steps: Vec<Rc<Json>>,
        previous_turn_cache: Option<CacheUsage>,
    }
    let mut usage = turn_tokens.then(|| Usage { steps: Vec::new(), previous_turn_cache: None });
    // Without any idle marker, history predates markers and a turn ends with its terminal step. With markers, steers
    // keep the turn open, so usage accumulates until the marker closes it.
    let legacy = !messages.iter().any(|m| m.str("type") == Some("idle"));
    let flush_turn = |rows: &mut Vec<ProjectionEntry>, usage: &mut Option<Usage>| {
        let Some(usage) = usage.as_mut() else { return };
        let steps: Vec<Rc<Json>> = usage.steps.drain(..).filter(|s| has_token_usage(s)).collect();
        let Some(last) = steps.last().cloned() else { return };
        rows.push(ProjectionEntry {
            entry: SessionEntry::TurnUsage {
                message_ids: steps.iter().map(|s| message_id(s).to_string()).collect(),
                previous_cache: usage.previous_turn_cache.clone(),
            },
            part: None,
            path: Vec::new(),
            closes_previous: None,
        });
        usage.previous_turn_cache = Some(CacheUsage {
            read: last.get("tokens").and_then(|t| t.get("cache")).map_or(0.0, |c| token(c, "read")),
            model: ModelRef::of(last.get("model")),
        });
    };
    // An execution failure that repeats the error of the turn's last step or compaction is already on screen.
    let mut shown_error: Option<(String, String)> = None;
    let ordered = messages
        .iter()
        .filter(|m| !pending.contains(message_id(m)))
        .chain(pending_compactions.iter().copied())
        .chain(messages.iter().filter(|m| is_input(m)));
    let mut rows: Vec<ProjectionEntry> = Vec::new();
    for message in ordered {
        let kind = message.str("type");
        if kind != Some("assistant") {
            if kind == Some("idle") {
                let error = error_identity(message.get("error"));
                if error.is_some() && error != shown_error {
                    rows.push(ProjectionEntry {
                        entry: SessionEntry::Message { message_id: message_id(message).to_string() },
                        part: None,
                        path: Vec::new(),
                        closes_previous: Some(true),
                    });
                }
                shown_error = None;
                flush_turn(&mut rows, &mut usage);
                continue;
            }
            if kind == Some("synthetic") && message.str("description").is_none_or(|d| d.trim().is_empty()) {
                continue;
            }
            if kind == Some("compaction") && message.str("status") != Some("running") {
                shown_error = if message.str("status") == Some("failed") { error_identity(message.get("error")) } else { None };
            }
            if kind == Some("compaction") && message.str("status") == Some("completed") {
                if let Some(usage) = usage.as_mut() {
                    usage.previous_turn_cache = None;
                }
            }
            rows.push(ProjectionEntry {
                entry: SessionEntry::Message { message_id: message_id(message).to_string() },
                part: None,
                path: message_path(message, verbosity),
                closes_previous: Some(!pending.contains(message_id(message))),
            });
            continue;
        }
        if let Some(usage) = usage.as_mut() {
            usage.steps.push(message.clone());
        }
        shown_error = error_identity(message.get("error"));
        let (mut texts, mut reasonings) = (0, 0);
        for part in message.arr("content").map(Vec::as_slice).unwrap_or(&[]) {
            let part_type = part.str("type").unwrap_or_default();
            let part_id = match part_type {
                "tool" => str_of(part, "id").to_string(),
                "text" => {
                    texts += 1;
                    format!("text:{}", texts - 1)
                }
                "reasoning" => {
                    reasonings += 1;
                    format!("reasoning:{}", reasonings - 1)
                }
                other => format!("{other}:0"),
            };
            if (part_type == "text" || part_type == "reasoning") && str_of(part, "text").trim().is_empty() {
                continue;
            }
            let appended = match part_type {
                "reasoning" => AppendPart::Reasoning { completed: part.get("time").and_then(|t| t.num("completed")) },
                "tool" => AppendPart::Tool { name: str_of(part, "name").to_string() },
                _ => AppendPart::Text,
            };
            let path = part_path(&appended, verbosity);
            rows.push(ProjectionEntry {
                entry: SessionEntry::Part { r#ref: PartRef::new(message_id(message), &part_id) },
                part: Some(appended),
                path,
                closes_previous: None,
            });
        }
        let finish = message.str("finish");
        let terminal = (finish.is_some_and(|f| f != "tool-calls" && f != "unknown")) || message.get("error").is_some_and(truthy);
        if terminal || message.get("retry").is_some_and(truthy) {
            rows.push(ProjectionEntry {
                entry: SessionEntry::AssistantFooter { message_id: message_id(message).to_string() },
                part: None,
                path: Vec::new(),
                closes_previous: None,
            });
        }
        if terminal && legacy {
            flush_turn(&mut rows, &mut usage);
        }
    }
    project_entries(rows)
}

/// `cacheReuseDrop(previous, current)`.
pub fn cache_reuse_drop(previous: Option<&CacheUsage>, current: &CacheUsage) -> Option<f64> {
    let previous = previous?;
    if previous.model != current.model {
        return None;
    }
    let drop = previous.read - current.read;
    // OpenAI cache reads can move between one and two 1,024-token buckets without a material loss of reuse.
    if current.model.provider_id == "openai" && (1024.0..=2048.0).contains(&drop) {
        return None;
    }
    (drop > 0.0).then_some(drop)
}

/// `messageBoundaryIDs(rows, messages)`.
pub fn message_boundary_ids(rows: &[SessionRow], messages: &[Rc<Json>]) -> Vec<Option<String>> {
    let by_id: std::collections::HashMap<&str, &Rc<Json>> = messages.iter().map(|m| (message_id(m), m)).collect();
    let mut seen = HashSet::new();
    rows.iter()
        .map(|row| {
            let id = row_boundary_message_id(row, &by_id)?;
            seen.insert(id.clone()).then_some(id)
        })
        .collect()
}

/// `sessionRowID(row, boundaryID)`.
pub fn session_row_id(row: &SessionRow, boundary_id: Option<&str>) -> Option<String> {
    if let Some(id) = boundary_id {
        return Some(id.to_string());
    }
    match row {
        SessionRow::Entry(SessionEntry::Part { r#ref }) => Some(format!("session-part:{}:{}", r#ref.message_id, r#ref.part_id)),
        _ => None,
    }
}

fn row_boundary_message_id(row: &SessionRow, messages: &std::collections::HashMap<&str, &Rc<Json>>) -> Option<String> {
    if let SessionRow::Entry(SessionEntry::Message { message_id: id }) = row {
        let message = messages.get(id.as_str())?;
        return (message.str("type") == Some("user") && !str_of(message, "text").trim().is_empty()).then(|| id.clone());
    }
    let id = match row {
        SessionRow::Entry(SessionEntry::Part { r#ref }) => Some(r#ref.message_id.clone()),
        SessionRow::Group(group) => group_refs(group, false).first().map(|r| r.message_id.clone()),
        SessionRow::Entry(SessionEntry::AssistantFooter { message_id }) => Some(message_id.clone()),
        SessionRow::Entry(SessionEntry::TurnUsage { message_ids, .. }) => message_ids.first().cloned(),
        _ => None,
    }?;
    let message = messages.get(id.as_str())?;
    (message.str("type") == Some("assistant")).then(|| message_id(message).to_string())
}

/// `resolvePart(message, partID)`.
pub fn resolve_part(message: &Json, part_id: &str) -> Option<Json> {
    let content = message.arr("content")?;
    if let Some(tool) = content.iter().find(|p| p.str("type") == Some("tool") && p.str("id") == Some(part_id)) {
        return Some(tool.clone());
    }
    let (kind, ordinal) = part_id.split_once(':')?;
    if kind != "text" && kind != "reasoning" {
        return None;
    }
    let ordinal: usize = ordinal.parse().ok().filter(|_| ordinal.chars().all(|c| c.is_ascii_digit()))?;
    content.iter().filter(|p| p.str("type") == Some(kind)).nth(ordinal).cloned()
}
