//! `handleEvent(event)` (solid/data.ts): how each server event changes the store, and the reads it sets off.

use std::rc::Rc;

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

use super::store::{self, Record};
use super::{Data, Inner, MessageChanges, Resource, SessionStatus, active_assistant, id, location_key, omit_file_data};
use crate::client::RequestOptions;
use crate::event::Event;
use crate::json::JsonExt;
use crate::obj;
use crate::reactive::{Key, LocationField};
use crate::runtime;

fn truthy(value: Option<&Json>) -> bool {
    value.is_some_and(crate::json::truthy)
}

fn str_of<'a>(value: &'a Json, key: &str) -> &'a str {
    value.str(key).unwrap_or("")
}

/// `existing.time.<key> = value` (creating `time` when absent, as a JS write would fail: TS rows always have one).
fn set_time(message: &mut Json, key: &str, value: Option<Json>) {
    if message.get("time").is_none() {
        message.set("time", Json::Object(Object::new()));
    }
    let time = message.get_mut("time").expect("time");
    match value {
        Some(value) => time.set(key, value),
        None => {
            time.remove(key);
        }
    }
}

/// `object.key = value`, where `undefined` stays a key holding `undefined` (absent from JSON).
fn assign(object: &mut Json, key: &str, value: Option<&Json>) {
    match value {
        Some(value) => object.set(key, value.clone()),
        None => {
            object.remove(key);
        }
    }
}

impl Data {
    pub(crate) fn handle_event(&self, event: &Event) {
        let inner = &self.inner;
        let data = event.data();
        let session = str_of(data, "sessionID").to_owned();
        let created = event.created().map(Json::Number);
        match event.r#type() {
            "server.connected" => {
                let updates = Rc::new(std::cell::RefCell::new(indexmap::IndexMap::new()));
                *inner.active_updates.borrow_mut() = Some(updates.clone());
                let this = self.clone();
                inner.refresh(move || {
                    let call = this.inner.api().api().session().active(&Json::Null, RequestOptions::default());
                    Box::pin(async move {
                        let inner = &this.inner;
                        let is_current = || inner.active_updates.borrow().as_ref().is_some_and(|u| Rc::ptr_eq(u, &updates));
                        let active = match call.await {
                            Ok(value) => value.json(),
                            Err(_) => {
                                if is_current() {
                                    *inner.active_updates.borrow_mut() = None;
                                }
                                return Ok(());
                            }
                        };
                        if !is_current() {
                            return Ok(());
                        }
                        // Lifecycle events received during hydration supersede the snapshot.
                        let mut snapshot: indexmap::IndexMap<String, SessionStatus> = match &active {
                            Json::Object(active) => active.keys().map(|id| (id.clone(), SessionStatus::Running)).collect(),
                            _ => indexmap::IndexMap::new(),
                        };
                        for (session, status) in updates.borrow().iter() {
                            match status {
                                None => {
                                    snapshot.shift_remove(session);
                                }
                                Some(status) => {
                                    snapshot.insert(session.clone(), *status);
                                }
                            }
                        }
                        *inner.active_updates.borrow_mut() = None;
                        inner.set(|store, keys| {
                            let entries: Vec<(String, SessionStatus)> = snapshot.iter().map(|(k, v)| (k.clone(), *v)).collect();
                            for changed in store::reconcile_map(&mut store.session.active, entries) {
                                keys.push(Key::Status(id(&changed)));
                            }
                        });
                        // Sessions created while disconnected never delivered `session.created`.
                        for (session, status) in &snapshot {
                            if *status == SessionStatus::Running && !inner.has_info(session) {
                                let api = this.session.clone();
                                let session = session.clone();
                                inner.refresh(move || Box::pin(api.sync(&session, false).wait()));
                            }
                        }
                        Ok(())
                    })
                });
                let this = self.clone();
                inner.refresh(move || {
                    let query = super::location_query(&this.inner.default_location());
                    let call = this.inner.api().api().location().get(&obj! { "location" => query }, RequestOptions::default());
                    Box::pin(async move {
                        let location = call.await?.json();
                        let key = location_key(&location);
                        this.inner.publish_location(&key, LocationField::Info, Some(location));
                        Ok(())
                    })
                });
                let vcs = self.location.vcs.clone();
                inner.refresh(move || Box::pin(vcs.sync(None).wait()));
                let project = self.project.clone();
                inner.refresh(move || Box::pin(project.sync().wait()));
                return;
            }
            "project.updated" => {
                let project = str_of(data, "id").to_owned();
                inner.set(|store, keys| {
                    let changed = match store.project.info.get_mut(&project) {
                        Some(existing) => store::reconcile(Rc::make_mut(existing), data.clone()),
                        None => {
                            store.project.info.insert(project.clone(), Rc::new(data.clone()));
                            true
                        }
                    };
                    if changed {
                        keys.push(Key::Project(id(&project)));
                    }
                });
                return;
            }
            "session.created" => {
                inner.clear_deleted(&session);
                inner.session_outbox.borrow_mut().shift_remove(&session);
                self.session.invalidate(&session);
                let api = self.session.clone();
                let target = session.clone();
                inner.refresh(move || Box::pin(api.sync(&target, false).wait()));
                // A newly created session starts empty, so live events can be its source of truth.
                inner.sync.complete(&format!("session.pending:{session}"));
                inner.sync.complete(&format!("session.message:{session}"));
                return;
            }
            "session.deleted" => {
                inner.mark_deleted(&session);
                return;
            }
            "session.usage.updated" => {
                self.merge_info(&session, &[("cost", data.get("cost")), ("tokens", data.get("tokens"))]);
                return;
            }
            "session.agent.selected" => {
                let previous = inner.info(&session).and_then(|info| info.get("agent").cloned());
                self.assign_info(&session, &["agent"], data.get("agent"));
                inner.insert_message(
                    &session,
                    obj! {
                        "id" => runtime::id::from_event(event.id()),
                        "type" => "agent-switched",
                        "agent" => data.get("agent"),
                        "previous" => previous,
                        "time" => obj! { "created" => created.clone() },
                    },
                );
                return;
            }
            "session.model.selected" => {
                let previous = data
                    .get("previous")
                    .filter(|p| !matches!(p, Json::Null))
                    .cloned()
                    .or_else(|| inner.info(&session).and_then(|info| info.get("model").cloned()));
                self.assign_info(&session, &["model"], data.get("model"));
                if !inner.store.borrow().session.message.contains_key(&session) {
                    return;
                }
                let message_id = runtime::id::from_event(event.id());
                inner.insert_message(
                    &session,
                    obj! {
                        "id" => message_id.as_str(),
                        "type" => "model-switched",
                        "model" => data.get("model"),
                        "previous" => previous,
                        "time" => obj! { "created" => created.clone() },
                    },
                );
                let this = self.clone();
                inner.refresh(move || {
                    let call = this.inner.api().api().session().message().get(
                        &obj! { "sessionID" => session.as_str(), "messageID" => message_id.as_str() },
                        RequestOptions::default(),
                    );
                    Box::pin(async move {
                        let item = call.await?.json();
                        let item_id = str_of(&item, "id").to_owned();
                        this.inner.update_messages(&session, |messages, index, changes| match index.get(&item_id).copied() {
                            None => Inner::append(messages, index, item, changes),
                            Some(position) => {
                                if position < messages.len() {
                                    messages[position] = Rc::new(item);
                                }
                                changes.message(&item_id);
                            }
                        });
                        Ok(())
                    })
                });
                return;
            }
            "session.renamed" => {
                // Preserve the live title when it races the session's initial read.
                let this = self.clone();
                let title = data.get("title").cloned();
                inner.refresh(move || {
                    let family = this.inner.sync.pending(&format!("session.family:{session}")).then(|| this.session.sync(&session, true));
                    let own = this.session.sync(&session, false);
                    Box::pin(async move {
                        super::api::all_settled_first_error(std::iter::once(own).chain(family)).await?;
                        this.assign_info(&session, &["title"], title.as_ref());
                        Ok(())
                    })
                });
                return;
            }
            "session.permissions" => {
                self.assign_info(&session, &["permissions"], data.get("permissions"));
                return;
            }
            "session.metadata.updated" => {
                // The event carries the whole record, so keys it drops must go too.
                inner.set(|store, keys| {
                    let Some(info) = store.session.info.get_mut(&session) else { return };
                    let info = Rc::make_mut(info);
                    let changed = match (info.get_mut("metadata"), data.get("metadata")) {
                        (Some(existing), Some(target)) => store::reconcile(existing, target.clone()),
                        (None, Some(target)) => {
                            info.set("metadata", target.clone());
                            true
                        }
                        (Some(_), None) => {
                            info.remove("metadata");
                            true
                        }
                        (None, None) => false,
                    };
                    if changed {
                        keys.push(Key::Session(id(&session)));
                    }
                });
                return;
            }
            "session.archived" => {
                self.assign_info(&session, &["time", "archived"], created.as_ref());
                return;
            }
            "session.unarchived" => {
                self.assign_info(&session, &["time", "archived"], None);
                return;
            }
            "session.moved" => {
                let Some(current) = inner.info(&session) else { return };
                let previous = obj! {
                    "location" => current.get("location").cloned().unwrap_or_else(crate::json::object),
                    "projectID" => current.get("projectID"),
                    "subpath" => current.get("subpath"),
                };
                self.assign_info(&session, &["location"], data.get("location"));
                self.assign_info(&session, &["projectID"], data.get("projectID"));
                self.assign_info(&session, &["subpath"], data.get("subpath"));
                inner.insert_message(
                    &session,
                    obj! {
                        "id" => runtime::id::from_event(event.id()),
                        "type" => "location-switched",
                        "location" => data.get("location"),
                        "projectID" => data.get("projectID"),
                        "subpath" => data.get("subpath"),
                        "previous" => previous,
                        "time" => obj! { "created" => created.clone() },
                    },
                );
                return;
            }
            "worktree.resolved" => {
                let infos: Vec<(String, Record)> =
                    inner.store.borrow().session.info.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                for (session, info) in infos {
                    let project = str_of(&info, "projectID").to_owned();
                    let explicit = data.arr("adopted").is_some_and(|adopted| adopted.iter().any(|p| p.as_str_opt() == Some(&project)));
                    let directory = if explicit {
                        inner.store.borrow().project.info.get(&project).and_then(|p| p.str("canonical").map(str::to_owned))
                    } else {
                        info.get("location").and_then(|l| l.str("directory").map(str::to_owned))
                    };
                    let Some(directory) = directory.filter(|d| !d.is_empty()) else {
                        self.session.invalidate(&session);
                        let api = self.session.clone();
                        inner.refresh(move || Box::pin(api.sync(&session, false).wait()));
                        continue;
                    };
                    let Some((project_id, subpath)) = adopt(&project, &directory, data) else { continue };
                    self.assign_info(&session, &["projectID"], Some(&Json::String(project_id)));
                    self.assign_info(&session, &["subpath"], subpath.map(Json::String).as_ref());
                }
                return;
            }
            "session.inbox.delivered" => {
                let inbox = str_of(data, "inboxID").to_owned();
                let admitted = self.session.input.has(&session, &inbox);
                inner.remove_pending(&session, Some(&inbox));
                inner.update_messages(&session, |messages, index, changes| {
                    let Some(position) = index.get(&inbox).copied() else { return };
                    if position >= messages.len() || !admitted {
                        return;
                    }
                    let mut existing = messages.remove(position);
                    set_time(Rc::make_mut(&mut existing), "created", created.clone());
                    messages.push(existing);
                    Inner::reindex(messages, index, position);
                    changes.ids();
                    changes.message(&inbox);
                });
                if let Some(active) = inner.compacting.borrow().get(&session) {
                    active.observed.borrow_mut().insert(inbox);
                }
                return;
            }
            "session.inbox.delivery.changed" => {
                inner.update_pending(&session, str_of(data, "inboxID"), str_of(data, "delivery"));
                return;
            }
            "session.inbox.cancelled" => {
                let inbox = str_of(data, "inboxID").to_owned();
                inner.retract_local(&session, &inbox);
                if let Some(active) = inner.compacting.borrow().get(&session) {
                    active.observed.borrow_mut().insert(inbox);
                }
                return;
            }
            "session.inbox.enqueued" => {
                let inbox = str_of(data, "inboxID").to_owned();
                inner.outbox.borrow_mut().shift_remove(&inbox);
                let mut item = obj! {
                    "id" => data.get("inboxID"),
                    "sessionID" => data.get("sessionID"),
                    "time" => obj! { "created" => created.clone() },
                };
                if let Some(Json::Object(fields)) = data.get("item") {
                    for (key, value) in fields {
                        item.set(key, value.clone());
                    }
                }
                let item = Rc::new(item);
                let item_session = str_of(&item, "sessionID").to_owned();
                if let Some(updates) = inner.pending_updates.borrow().get(&item_session) {
                    updates.borrow_mut().insert(str_of(&item, "id").to_owned(), super::PendingUpdate::Item(item.clone()));
                }
                inner.admit_local(item);
                if data.get("item").and_then(|i| i.str("type")) == Some("compaction") {
                    let retire = {
                        let compacting = inner.compacting.borrow();
                        let active = compacting.get(&session);
                        if let Some(active) = active {
                            active.observed.borrow_mut().insert(inbox.clone());
                        }
                        active.filter(|a| a.id != inbox).map(|a| a.id.clone())
                    };
                    if let Some(retire) = retire {
                        if inner.outbox.borrow_mut().shift_remove(&retire) {
                            inner.remove_pending(&session, Some(&retire));
                        }
                    }
                }
                return;
            }
            "session.instructions.updated" => {
                // The initial baseline and empty-rendering deltas carry no text and produce no message.
                let Some(text) = data.get("text") else { return };
                let delta = match data.get("delta") {
                    Some(Json::Object(delta)) => delta.keys().cloned().collect::<Vec<_>>().join(", "),
                    _ => String::new(),
                };
                inner.insert_message(
                    &session,
                    obj! {
                        "id" => runtime::id::from_event(event.id()),
                        "type" => "system",
                        "text" => text,
                        "description" => format!("Instructions updated: {delta}"),
                        "metadata" => event.metadata(),
                        "time" => obj! { "created" => created.clone() },
                    },
                );
                return;
            }
            "session.synthetic" => {
                inner.insert_message(
                    &session,
                    obj! {
                        "id" => runtime::id::from_event(event.id()),
                        "type" => "synthetic",
                        "text" => data.get("text"),
                        "description" => data.get("description"),
                        "metadata" => data.get("metadata"),
                        "time" => obj! { "created" => created.clone() },
                    },
                );
                return;
            }
            "session.shell.started" => {
                let shell = data.get("shell").cloned().unwrap_or_default();
                let background = shell.get("metadata").and_then(|m| m.get("background")) == Some(&Json::Bool(true));
                let metadata = if background {
                    let mut metadata = match event.metadata() {
                        Some(Json::Object(m)) => Json::Object(m.clone()),
                        _ => crate::json::object(),
                    };
                    metadata.set("background", Json::Bool(true));
                    Some(metadata)
                } else {
                    event.metadata().cloned()
                };
                inner.insert_message(
                    &session,
                    obj! {
                        "id" => runtime::id::from_event(event.id()),
                        "type" => "shell",
                        "shellID" => shell.get("id"),
                        "command" => shell.get("command"),
                        "status" => shell.get("status"),
                        "exit" => shell.get("exit"),
                        "metadata" => metadata,
                        "time" => obj! { "created" => created.clone() },
                    },
                );
                return;
            }
            "session.shell.ended" => {
                if !inner.store.borrow().session.message.contains_key(&session) {
                    return;
                }
                let shell = data.get("shell").cloned().unwrap_or_default();
                inner.update_messages(&session, |messages, _, changes| {
                    let Some(position) = messages
                        .iter()
                        .rposition(|item| item.str("type") == Some("shell") && item.get("shellID") == shell.get("id"))
                    else {
                        return;
                    };
                    let item = Rc::make_mut(&mut messages[position]);
                    assign(item, "status", shell.get("status"));
                    assign(item, "exit", shell.get("exit"));
                    assign(item, "output", data.get("output"));
                    set_time(item, "completed", created.clone());
                    let message_id = str_of(item, "id").to_owned();
                    changes.message(&message_id);
                });
                return;
            }
            "session.step.started" => {
                let assistant = str_of(data, "assistantMessageID").to_owned();
                inner.update_messages(&session, |messages, index, changes| {
                    let position = index.get(&assistant).copied();
                    let existing = position.and_then(|p| messages.get_mut(p)).filter(|m| m.str("type") == Some("assistant"));
                    if let Some(existing) = existing {
                        let existing = Rc::make_mut(existing);
                        assign(existing, "agent", data.get("agent"));
                        assign(existing, "model", data.get("model"));
                        for key in ["retry", "error", "finish", "rawFinish", "native"] {
                            existing.remove(key);
                        }
                        set_time(existing, "created", data.get("started").cloned());
                        set_time(existing, "streamed", None);
                        set_time(existing, "completed", None);
                        if truthy(data.get("snapshot")) {
                            let mut snapshot = match existing.get("snapshot") {
                                Some(Json::Object(s)) => Json::Object(s.clone()),
                                _ => crate::json::object(),
                            };
                            snapshot.set("start", data.get("snapshot").cloned().unwrap_or_default());
                            existing.set("snapshot", snapshot);
                        }
                        changes.head(&assistant);
                        return;
                    }
                    if let Some(current) = active_assistant(messages) {
                        let item = Rc::make_mut(&mut messages[current]);
                        item.remove("retry");
                        let completed = item.get("time").and_then(|t| t.get("completed")).filter(|c| !matches!(c, Json::Null)).is_some();
                        if !completed {
                            set_time(item, "completed", created.clone());
                        }
                        let current_id = str_of(item, "id").to_owned();
                        changes.head(&current_id);
                    }
                    Inner::append(
                        messages,
                        index,
                        obj! {
                            "id" => assistant.as_str(),
                            "type" => "assistant",
                            "agent" => data.get("agent"),
                            "model" => data.get("model"),
                            "metadata" => event.metadata(),
                            "content" => Vec::new(),
                            "snapshot" => truthy(data.get("snapshot")).then(|| obj! { "start" => data.get("snapshot") }),
                            "time" => obj! { "created" => data.get("started") },
                        },
                        changes,
                    );
                });
                return;
            }
            "session.step.streamed" => {
                let assistant = str_of(data, "assistantMessageID").to_owned();
                inner.edit_assistant(&session, &assistant, |item, changes| {
                    set_time(item, "streamed", created.clone());
                    changes.head(&assistant);
                });
                return;
            }
            "session.step.ended" => {
                let assistant = str_of(data, "assistantMessageID").to_owned();
                inner.edit_assistant(&session, &assistant, |item, changes| {
                    set_time(item, "completed", created.clone());
                    for key in ["finish", "rawFinish", "native", "cost", "tokens"] {
                        assign(item, key, data.get(key));
                    }
                    if truthy(data.get("snapshot")) {
                        let mut snapshot = match item.get("snapshot") {
                            Some(Json::Object(s)) => Json::Object(s.clone()),
                            _ => crate::json::object(),
                        };
                        snapshot.set("end", data.get("snapshot").cloned().unwrap_or_default());
                        item.set("snapshot", snapshot);
                    }
                    changes.head(&assistant);
                });
                return;
            }
            "session.step.failed" => {
                // A budget refusal means the cached usage limits are out of date.
                let kind = data.get("error").and_then(|e| e.str("type"));
                if let Some(location) = event.location() {
                    if matches!(kind, Some("provider.budget-limit") | Some("provider.model-budget-limit")) {
                        inner.refresh_usage_limits(location);
                    }
                }
                let assistant = str_of(data, "assistantMessageID").to_owned();
                inner.edit_assistant(&session, &assistant, |item, changes| {
                    set_time(item, "completed", created.clone());
                    let finish = data.get("finish").filter(|f| !matches!(f, Json::Null)).cloned().unwrap_or_else(|| Json::String("error".into()));
                    item.set("finish", finish);
                    assign(item, "rawFinish", data.get("rawFinish"));
                    assign(item, "native", data.get("native"));
                    assign(item, "error", data.get("error"));
                    item.remove("retry");
                    if data.get("cost").is_some() && data.get("tokens").is_some() {
                        assign(item, "cost", data.get("cost"));
                        assign(item, "tokens", data.get("tokens"));
                    }
                    changes.head(&assistant);
                });
                return;
            }
            "session.text.started" => {
                let assistant = str_of(data, "assistantMessageID").to_owned();
                inner.edit_assistant(&session, &assistant, |item, changes| push_part(item, obj! { "type" => "text", "text" => "" }, &assistant, changes));
                return;
            }
            "session.text.delta" => {
                let delta = data.str("delta").map(str::to_owned).unwrap_or_else(|| data.get("delta").map(crate::js::string).unwrap_or_else(|| "undefined".into()));
                inner.edit_part(&session, str_of(data, "assistantMessageID"), |p| p.str("type") == Some("text"), |text| append_text(text, "text", &delta));
                return;
            }
            "session.text.ended" => {
                inner.edit_part(&session, str_of(data, "assistantMessageID"), |p| p.str("type") == Some("text"), |text| assign(text, "text", data.get("text")));
                return;
            }
            "session.tool.input.started" => {
                let assistant = str_of(data, "assistantMessageID").to_owned();
                inner.edit_assistant(&session, &assistant, |item, changes| {
                    push_part(
                        item,
                        obj! {
                            "type" => "tool",
                            "id" => data.get("id"),
                            "name" => data.get("name"),
                            "time" => obj! { "created" => created.clone() },
                            "state" => obj! { "status" => "streaming", "input" => "" },
                        },
                        &assistant,
                        changes,
                    )
                });
                return;
            }
            "session.tool.input.delta" => {
                let delta = data.get("delta").map(crate::js::string).unwrap_or_else(|| "undefined".into());
                self.edit_tool(&session, data, |tool| {
                    if tool_status(tool) == Some("streaming") {
                        let state = tool.get_mut("state").expect("state");
                        append_text(state, "input", &delta);
                    }
                });
                return;
            }
            "session.tool.input.ended" => {
                self.edit_tool(&session, data, |tool| {
                    if tool_status(tool) == Some("streaming") {
                        assign(tool.get_mut("state").expect("state"), "input", data.get("text"));
                    }
                });
                return;
            }
            "session.tool.called" => {
                self.edit_tool(&session, data, |tool| {
                    if tool.get("time").is_none() {
                        tool.set("time", crate::json::object());
                    }
                    assign(tool.get_mut("time").expect("time"), "ran", created.as_ref());
                    assign(tool, "executed", data.get("executed"));
                    assign(tool, "native", data.get("native"));
                    tool.set("state", obj! { "status" => "running", "input" => data.get("input"), "metadata" => obj! {} });
                });
                return;
            }
            "session.tool.progress" => {
                self.edit_tool(&session, data, |tool| {
                    if tool_status(tool) == Some("running") {
                        assign(tool.get_mut("state").expect("state"), "metadata", data.get("metadata"));
                    }
                });
                return;
            }
            "session.tool.success" => {
                let omit = inner.omit_tool_file_data;
                self.edit_tool(&session, data, |tool| {
                    if tool_status(tool) != Some("running") {
                        return;
                    }
                    let input = tool.get("state").and_then(|s| s.get("input")).cloned();
                    let content = data.get("content").map(|c| if omit { omit_file_data(c) } else { c.clone() });
                    tool.set(
                        "state",
                        obj! { "status" => "completed", "input" => input, "metadata" => data.get("metadata"), "content" => content },
                    );
                    let executed = truthy(data.get("executed")) || tool.get("executed") == Some(&Json::Bool(true));
                    tool.set("executed", Json::Bool(executed));
                    merge_native(tool, data.get("native"));
                    set_time(tool, "completed", created.clone());
                });
                return;
            }
            "session.tool.failed" => {
                let omit = inner.omit_tool_file_data;
                self.edit_tool(&session, data, |tool| {
                    if !matches!(tool_status(tool), Some("streaming") | Some("running")) {
                        return;
                    }
                    let input = match tool.get("state").and_then(|s| s.get("input")) {
                        Some(Json::String(_)) => Some(crate::json::object()),
                        other => other.cloned(),
                    };
                    let content = match data.get("content") {
                        Some(content) if omit && crate::json::truthy(content) => Some(omit_file_data(content)),
                        other => other.cloned(),
                    };
                    tool.set(
                        "state",
                        obj! {
                            "status" => "error",
                            "error" => data.get("error"),
                            "input" => input,
                            "metadata" => data.get("metadata"),
                            "content" => content,
                        },
                    );
                    let executed = truthy(data.get("executed")) || tool.get("executed") == Some(&Json::Bool(true));
                    tool.set("executed", Json::Bool(executed));
                    merge_native(tool, data.get("native"));
                    set_time(tool, "completed", created.clone());
                });
                return;
            }
            "session.reasoning.started" => {
                let assistant = str_of(data, "assistantMessageID").to_owned();
                inner.edit_assistant(&session, &assistant, |item, changes| {
                    push_part(
                        item,
                        obj! { "type" => "reasoning", "text" => "", "native" => data.get("native"), "time" => obj! { "created" => created.clone() } },
                        &assistant,
                        changes,
                    )
                });
                return;
            }
            "session.reasoning.delta" => {
                let delta = data.get("delta").map(crate::js::string).unwrap_or_else(|| "undefined".into());
                inner.edit_part(&session, str_of(data, "assistantMessageID"), open_reasoning, |reasoning| append_text(reasoning, "text", &delta));
                return;
            }
            "session.reasoning.ended" => {
                inner.edit_part(&session, str_of(data, "assistantMessageID"), open_reasoning, |reasoning| {
                    assign(reasoning, "text", data.get("text"));
                    let started = reasoning.get("time").and_then(|t| t.get("created")).filter(|c| !matches!(c, Json::Null)).cloned().or(created.clone());
                    reasoning.set("time", obj! { "created" => started, "completed" => created.clone() });
                    if data.get("native").is_some() {
                        assign(reasoning, "native", data.get("native"));
                    }
                });
                return;
            }
            "session.retry.scheduled" => {
                let assistant = str_of(data, "assistantMessageID").to_owned();
                inner.edit_assistant(&session, &assistant, |item, changes| {
                    item.set("retry", obj! { "attempt" => data.get("attempt"), "at" => data.get("at"), "error" => data.get("error") });
                    changes.head(&assistant);
                });
                return;
            }
            "session.execution.started" => {
                inner.set_session_active(&session, SessionStatus::Running);
                // Revalidate so `time.busy` follows the projected execution start.
                if !inner.has_info(&session) && !inner.sync.has(&format!("session:{session}")) {
                    return;
                }
                self.session.invalidate(&session);
                let api = self.session.clone();
                inner.refresh(move || Box::pin(api.sync(&session, false).wait()));
                return;
            }
            "session.compaction.started" => {
                let input = data.str("inputID").map(str::to_owned).filter(|i| !i.is_empty());
                if let Some(input) = &input {
                    inner.remove_pending(&session, Some(input));
                }
                // `inputID ?? fromEvent(id)`: only a missing ID falls back.
                let message_id = data.get("inputID").filter(|i| !matches!(i, Json::Null)).cloned();
                inner.insert_message(
                    &session,
                    obj! {
                        "id" => message_id.unwrap_or_else(|| Json::String(runtime::id::from_event(event.id()))),
                        "type" => "compaction",
                        "status" => "running",
                        "reason" => data.get("reason"),
                        "summary" => "",
                        "time" => obj! { "created" => created.clone() },
                    },
                );
                if let Some(input) = input {
                    if let Some(active) = inner.compacting.borrow().get(&session) {
                        active.observed.borrow_mut().insert(input);
                    }
                }
                return;
            }
            "session.execution.succeeded" | "session.execution.failed" | "session.execution.interrupted" => {
                let kind = event.r#type();
                inner.set_session_active(&session, SessionStatus::Idle);
                inner.update_messages(&session, |messages, _, changes| {
                    if let Some(current) = active_assistant(messages) {
                        let item = Rc::make_mut(&mut messages[current]);
                        if item.remove("retry").is_some() {
                            let item_id = str_of(item, "id").to_owned();
                            changes.head(&item_id);
                        }
                    }
                });
                if kind == "session.execution.interrupted" && data.str("reason") == Some("shutdown") {
                    return;
                }
                // Mirror the projected idle marker so turn boundaries match before the next message read.
                let outcome = match kind {
                    "session.execution.succeeded" => "succeeded",
                    "session.execution.failed" => "failed",
                    _ => "interrupted",
                };
                let mut idle = obj! { "id" => runtime::id::from_event(event.id()), "type" => "idle", "outcome" => outcome };
                if kind == "session.execution.failed" {
                    assign(&mut idle, "error", data.get("error"));
                }
                idle.set("time", obj! { "created" => created.clone() });
                inner.insert_message(&session, idle);
                let open_tool = inner.store.borrow().session.message.get(&session).is_some_and(|messages| {
                    messages.iter().any(|item| {
                        item.str("type") == Some("assistant")
                            && item.arr("content").is_some_and(|content| {
                                content.iter().any(|part| {
                                    part.str("type") == Some("tool") && matches!(tool_status(part), Some("streaming") | Some("running"))
                                })
                            })
                    })
                });
                if open_tool {
                    inner.sync.invalidate(Some(&format!("session.message:{session}")));
                    let messages = self.session.message.clone();
                    let target = session.clone();
                    inner.refresh(move || Box::pin(messages.sync(&target).wait()));
                }
                // An event can overtake the first read; queue a revalidation when that read is still active.
                if !inner.has_info(&session) && !inner.sync.has(&format!("session:{session}")) {
                    return;
                }
                self.session.invalidate(&session);
                let api = self.session.clone();
                inner.refresh(move || Box::pin(api.sync(&session, false).wait()));
                return;
            }
            "session.viewed" => {
                if !inner.has_info(&session) && !inner.sync.has(&format!("session:{session}")) {
                    return;
                }
                self.session.invalidate(&session);
                let api = self.session.clone();
                inner.refresh(move || Box::pin(api.sync(&session, false).wait()));
                return;
            }
            "session.revert.staged" => {
                self.assign_info(&session, &["revert"], data.get("revert"));
                return;
            }
            "session.revert.cleared" => {
                self.assign_info(&session, &["revert"], None);
                return;
            }
            "session.revert.committed" => {
                self.assign_info(&session, &["revert"], None);
                // The projector also deletes inbox items enqueued at or after the boundary without a cancel event.
                let to = str_of(data, "to").to_owned();
                inner.set(|store, keys| {
                    let kept: Vec<Record> = store
                        .session
                        .pending
                        .get(&session)
                        .map(|p| p.iter().filter(|item| str_of(item, "id") < to.as_str()).cloned().collect())
                        .unwrap_or_default();
                    store.session.pending.insert(session.clone(), Rc::new(kept));
                    keys.push(Key::Pending(id(&session)));
                });
                inner.update_messages(&session, |messages, index, changes| {
                    let Some(position) = messages.iter().position(|item| str_of(item, "id") >= to.as_str()) else { return };
                    for item in messages.drain(position..) {
                        index.remove(str_of(&item, "id"));
                    }
                    changes.all();
                });
                return;
            }
            "session.compaction.delta" => {
                if !inner.store.borrow().session.message.contains_key(&session) {
                    return;
                }
                let text = data.get("text").map(crate::js::string).unwrap_or_else(|| "undefined".into());
                inner.update_messages(&session, |messages, _, changes| {
                    let Some(position) =
                        messages.iter().rposition(|item| item.str("type") == Some("compaction") && item.str("status") == Some("running"))
                    else {
                        return;
                    };
                    let item = Rc::make_mut(&mut messages[position]);
                    append_text(item, "summary", &text);
                    let item_id = str_of(item, "id").to_owned();
                    changes.message(&item_id);
                });
                return;
            }
            "session.compaction.ended" => {
                let event_id = runtime::id::from_event(event.id());
                inner.update_messages(&session, |messages, index, changes| {
                    let position =
                        messages.iter().rposition(|item| item.str("type") == Some("compaction") && item.str("status") == Some("running"));
                    if let Some(position) = position {
                        let item = Rc::make_mut(&mut messages[position]);
                        item.set("status", Json::String("completed".into()));
                        for (key, from) in [("reason", "reason"), ("model", "model"), ("native", "native"), ("summary", "text"), ("cost", "cost"), ("tokens", "tokens")] {
                            // `Object.assign` sets each key, `undefined` included.
                            match data.get(from) {
                                Some(value) => item.set(key, value.clone()),
                                None => {
                                    item.remove(key);
                                }
                            }
                        }
                        let item_id = str_of(item, "id").to_owned();
                        changes.message(&item_id);
                        return;
                    }
                    Inner::append(
                        messages,
                        index,
                        obj! {
                            "id" => event_id.as_str(),
                            "type" => "compaction",
                            "status" => "completed",
                            "reason" => data.get("reason"),
                            "model" => data.get("model"),
                            "native" => data.get("native"),
                            "summary" => data.get("text"),
                            "cost" => data.get("cost"),
                            "tokens" => data.get("tokens"),
                            "time" => obj! { "created" => created.clone() },
                        },
                        changes,
                    );
                });
                return;
            }
            "session.compaction.failed" => {
                let input = data.str("inputID").map(str::to_owned).filter(|i| !i.is_empty());
                if let Some(input) = &input {
                    inner.remove_pending(&session, Some(input));
                }
                let event_id = runtime::id::from_event(event.id());
                inner.update_messages(&session, |messages, index, changes| {
                    let position =
                        messages.iter().rposition(|item| item.str("type") == Some("compaction") && item.str("status") == Some("running"));
                    let current = position.map(|p| messages[p].clone());
                    let failed = obj! {
                        "id" => current
                            .as_ref()
                            .and_then(|c| c.get("id").filter(|i| !matches!(i, Json::Null)).cloned())
                            .or_else(|| data.get("inputID").filter(|i| !matches!(i, Json::Null)).cloned())
                            .unwrap_or_else(|| Json::String(event_id.clone())),
                        "type" => "compaction",
                        "status" => "failed",
                        "reason" => data.get("reason"),
                        "error" => data.get("error"),
                        "metadata" => match &current { Some(c) => c.get("metadata").cloned(), None => event.metadata().cloned() },
                        "cost" => data.get("cost"),
                        "tokens" => data.get("tokens"),
                        "time" => match &current { Some(c) => c.get("time").cloned(), None => Some(obj! { "created" => created.clone() }) },
                    };
                    match position {
                        Some(position) => {
                            let failed_id = str_of(&failed, "id").to_owned();
                            messages[position] = Rc::new(failed);
                            changes.message(&failed_id);
                        }
                        None => Inner::append(messages, index, failed, changes),
                    }
                });
                if let Some(input) = input {
                    if let Some(active) = inner.compacting.borrow().get(&session) {
                        active.observed.borrow_mut().insert(input);
                    }
                }
                return;
            }
            "permission.asked" => {
                let request = str_of(data, "id").to_owned();
                inner.set(|store, keys| {
                    let list = store.session.permission.entry(session.clone()).or_insert_with(|| Rc::new(Vec::new()));
                    if list.iter().any(|r| r.str("id") == Some(&request)) {
                        return;
                    }
                    Rc::make_mut(list).push(Rc::new(data.clone()));
                    keys.push(Key::Permission(id(&session)));
                });
                return;
            }
            "permission.replied" => {
                inner.remove_permission(&session, str_of(data, "requestID"));
                return;
            }
            "form.replied" | "form.cancelled" => {
                inner.remove_form(&session, str_of(data, "id"), event.location());
                return;
            }
            _ => {}
        }

        if matches!(event.r#type(), "credential.updated" | "credential.switched") {
            let keys: Vec<String> = inner.store.borrow().location.keys().cloned().collect();
            for key in keys {
                let reference: Json = opencode_protocol::wire::parse(&key).unwrap_or_default();
                let directory = match &reference {
                    Json::Array(parts) => parts.first().cloned().unwrap_or_default(),
                    _ => Json::Null,
                };
                let workspace = match &reference {
                    Json::Array(parts) => parts.get(1).cloned().filter(|w| !matches!(w, Json::Null)),
                    _ => None,
                };
                let location = obj! { "directory" => directory, "workspaceID" => workspace };
                inner.refresh_usage_limits(&location);
                if event.r#type() == "credential.updated" {
                    inner.refresh_location(Resource::new(inner, LocationField::Integration), &location);
                    continue;
                }
                let integration = inner.store.borrow().location.get(&key).and_then(|d| d.get(LocationField::Integration).cloned());
                if let Some(Json::Array(integrations)) = integration.as_deref() {
                    let reordered: Vec<Json> = integrations
                        .iter()
                        .map(|integration| {
                            if integration.get("id") != data.get("integrationID") {
                                return integration.clone();
                            }
                            let connections = integration.arr("connections").cloned().unwrap_or_default();
                            let active = connections.iter().position(|c| {
                                c.str("type") == Some("credential") && c.get("id") == data.get("credentialID")
                            });
                            let Some(active) = active else { return integration.clone() };
                            let mut ordered = vec![connections[active].clone()];
                            ordered.extend(connections.iter().enumerate().filter(|(i, _)| *i != active).map(|(_, c)| c.clone()));
                            let mut next = integration.clone();
                            next.set("connections", Json::Array(ordered));
                            next
                        })
                        .collect();
                    inner.publish_location(&key, LocationField::Integration, Some(Json::Array(reordered)));
                }
                let active = inner.has_location(LocationField::Integration, Some(&location))
                    || inner.has_location(LocationField::Model, Some(&location))
                    || inner.has_location(LocationField::Provider, Some(&location));
                self.location.model.invalidate(Some(&location));
                self.location.provider.invalidate(Some(&location));
                if active {
                    let model = self.location.model.clone();
                    let provider = self.location.provider.clone();
                    inner.refresh(move || {
                        let all = [model.sync(Some(&location)), provider.sync(Some(&location))];
                        Box::pin(super::api::all_settled_first_error(all))
                    });
                }
            }
            return;
        }

        let Some(location) = event.location().cloned() else { return };
        let resource = |field| Resource::new(inner, field);
        match event.r#type() {
            "provider.updated" => inner.refresh_location(resource(LocationField::Provider), &location),
            "model.updated" => inner.refresh_location(resource(LocationField::Model), &location),
            "agent.updated" => inner.refresh_location(resource(LocationField::Agent), &location),
            "command.updated" => inner.refresh_location(resource(LocationField::Command), &location),
            "skill.updated" => inner.refresh_location(resource(LocationField::Skill), &location),
            // The event names only the current branch: reload the rest too.
            "vcs.branch.updated" => inner.refresh_location(resource(LocationField::Vcs), &location),
            "form.created" => {
                let form = data.get("form").cloned().unwrap_or_default();
                let owner = str_of(&form, "sessionID").to_owned();
                inner.set(|store, keys| {
                    let list = store.session.form.entry(owner.clone()).or_insert_with(|| Rc::new(Vec::new()));
                    if list.iter().any(|f| f.get("id") == form.get("id")) {
                        return;
                    }
                    let mut entry = form.clone();
                    if owner == "global" {
                        entry.set("location", location.clone());
                    }
                    Rc::make_mut(list).push(Rc::new(entry));
                    keys.push(Key::Form(id(&owner)));
                });
            }
            "shell.created" => {
                let key = location_key(&location);
                let info = data.get("info").cloned().unwrap_or_default();
                let shell = info.get("id").map(crate::js::string).unwrap_or_else(|| "undefined".into());
                let current = inner.store.borrow().location.get(&key).and_then(|d| d.get(LocationField::Shell).cloned());
                let mut shells = match current.as_deref() {
                    Some(Json::Object(shells)) => Json::Object(shells.clone()),
                    _ => crate::json::object(),
                };
                let mut entry = info;
                entry.set("location", location.clone());
                shells.set(&shell, entry);
                inner.publish_location(&key, LocationField::Shell, Some(shells));
            }
            "shell.exited" | "shell.deleted" => {
                let key = location_key(&location);
                let shell = data.get("id").map(crate::js::string).unwrap_or_else(|| "undefined".into());
                let current = inner.store.borrow().location.get(&key).and_then(|d| d.get(LocationField::Shell).cloned());
                let Some(Json::Object(shells)) = current.as_deref() else { return };
                if !shells.contains_key(&shell) {
                    return;
                }
                let kept: Object = shells.iter().filter(|(id, _)| **id != shell).map(|(k, v)| (k.clone(), v.clone())).collect();
                inner.publish_location(&key, LocationField::Shell, Some(Json::Object(kept)));
            }
            "reference.updated" => inner.refresh_location(resource(LocationField::Reference), &location),
            "integration.updated" => {
                inner.refresh_usage_limits(&location);
                let active = inner.has_location(LocationField::Integration, Some(&location))
                    || inner.has_location(LocationField::Model, Some(&location))
                    || inner.has_location(LocationField::Provider, Some(&location));
                self.location.integration.invalidate(Some(&location));
                self.location.model.invalidate(Some(&location));
                self.location.provider.invalidate(Some(&location));
                if active {
                    let (integration, model, provider) =
                        (self.location.integration.clone(), self.location.model.clone(), self.location.provider.clone());
                    inner.refresh(move || {
                        let all = [integration.sync(Some(&location)), model.sync(Some(&location)), provider.sync(Some(&location))];
                        Box::pin(super::api::all_settled_first_error(all))
                    });
                }
            }
            "config.updated" => {
                inner.refresh_location(resource(LocationField::Config), &location);
                if inner.has_location(LocationField::Websearch, Some(&location)) {
                    let websearch = self.location.websearch.clone();
                    inner.refresh(move || Box::pin(websearch.refresh(Some(&location)).wait()));
                }
            }
            "websearch.updated" => {
                if inner.has_location(LocationField::Websearch, Some(&location)) {
                    let websearch = self.location.websearch.clone();
                    inner.refresh(move || Box::pin(websearch.refresh(Some(&location)).wait()));
                }
            }
            // One event per MCP server as each settles: refetch once the burst goes quiet.
            "mcp.status.changed" => {
                let active = inner.has_location(LocationField::McpServer, Some(&location));
                self.location.mcp.server.invalidate(Some(&location));
                if active {
                    let server = self.location.mcp.server.clone();
                    let key = format!("mcp.status:{}", location_key(&location));
                    inner.settle(&key, move || Box::pin(server.sync(Some(&location)).wait()));
                }
            }
            "mcp.resources.changed" => inner.refresh_location(resource(LocationField::McpResource), &location),
            _ => {}
        }
    }

    /// `setStore("session", "info", sessionID, ...path, value)` when the info is loaded; `None` deletes.
    fn assign_info(&self, session: &str, path: &[&str], value: Option<&Json>) {
        self.inner.set(|store, keys| {
            let Some(info) = store.session.info.get_mut(session) else { return };
            let mut target: &Json = info;
            for key in &path[..path.len() - 1] {
                match target.get(key) {
                    Some(next) => target = next,
                    None => return,
                }
            }
            let last = path[path.len() - 1];
            if target.get(last) == value {
                return;
            }
            let mut slot: &mut Json = Rc::make_mut(info);
            for key in &path[..path.len() - 1] {
                slot = slot.get_mut(key).expect("checked above");
            }
            // An object set over an object merges into it (Solid's `mergeStoreNode`): keys it lacks stay.
            match (slot.get_mut(last), value) {
                (Some(existing @ Json::Object(_)), Some(Json::Object(fields))) => {
                    store::merge(existing, fields);
                }
                _ => assign(slot, last, value),
            }
            keys.push(Key::Session(id(session)));
        });
    }

    /// `setStore("session", "info", sessionID, { ...fields })` when the info is loaded (a merge).
    fn merge_info(&self, session: &str, fields: &[(&str, Option<&Json>)]) {
        self.inner.set(|store, keys| {
            let Some(info) = store.session.info.get_mut(session) else { return };
            let mut changed = false;
            for (key, value) in fields {
                if info.get(key) != *value {
                    assign(Rc::make_mut(info), key, *value);
                    changed = true;
                }
            }
            if changed {
                keys.push(Key::Session(id(session)));
            }
        });
    }

    /// `message.editTool(sessionID, messageID, toolID, fn)`.
    fn edit_tool(&self, session: &str, data: &Json, f: impl FnOnce(&mut Json)) {
        let tool = data.get("id").cloned();
        self.inner.edit_part(
            session,
            str_of(data, "assistantMessageID"),
            move |part| part.str("type") == Some("tool") && part.get("id") == tool.as_ref(),
            f,
        );
    }
}

/// `assistant.content.push(part)`.
fn push_part(item: &mut Json, part: Json, message: &str, changes: &mut MessageChanges) {
    if item.get("content").is_none() {
        item.set("content", Json::Array(Vec::new()));
    }
    if let Some(Json::Array(content)) = item.get_mut("content") {
        content.push(part);
        let index = content.len() - 1;
        changes.parts(message);
        changes.part(message, index);
    }
}

/// `value.key += text` (a missing key reads as `undefined`).
fn append_text(value: &mut Json, key: &str, text: &str) {
    let current = match value.get(key) {
        Some(Json::String(s)) => s.clone(),
        Some(other) => crate::js::string(other),
        None => "undefined".into(),
    };
    value.set(key, Json::String(current + text));
}

fn tool_status(tool: &Json) -> Option<&str> {
    tool.get("state").and_then(|s| s.str("status"))
}

/// The open reasoning part (`!item.time?.completed`).
fn open_reasoning(part: &Json) -> bool {
    part.str("type") == Some("reasoning") && !truthy(part.get("time").and_then(|t| t.get("completed")))
}

/// `tool.native = { ...tool.native, ...native }` when the event has one.
fn merge_native(tool: &mut Json, native: Option<&Json>) {
    let Some(native) = native.filter(|n| crate::json::truthy(n)) else { return };
    let mut merged = match tool.get("native") {
        Some(Json::Object(own)) => Json::Object(own.clone()),
        _ => crate::json::object(),
    };
    if let Json::Object(fields) = native {
        for (key, value) in fields {
            merged.set(key, value.clone());
        }
    }
    tool.set("native", merged);
}

trait StrOpt {
    fn as_str_opt(&self) -> Option<&str>;
}

impl StrOpt for Json {
    fn as_str_opt(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }
}

/// `Worktree.adopt(session, event)`: the session's new project and subpath when the resolved worktree adopts it.
fn adopt(project: &str, directory: &str, event: &Json) -> Option<(String, Option<String>)> {
    let event_project = str_of(event, "projectID");
    let previous = str_of(event, "previous");
    let adopted = event.arr("adopted").is_some_and(|a| a.iter().any(|p| p.as_str_opt() == Some(project)));
    if project != previous && project != "global" && !adopted {
        return None;
    }
    if project == event_project {
        return None;
    }
    let normalize = |value: &str| {
        value.replace('\\', "/").split('/').fold(String::new(), |result, segment| {
            if segment.is_empty() || segment == "." {
                return result;
            }
            if segment == ".." {
                return match result.rfind('/') {
                    Some(at) => result[..at].to_owned(),
                    None => {
                        let mut chars = result.chars();
                        chars.next_back();
                        chars.as_str().to_owned()
                    }
                };
            }
            format!("{result}/{segment}")
        })
    };
    let directory = normalize(directory);
    let root = normalize(str_of(event, "directory"));
    let windows = root.len() >= 3 && root.as_bytes()[0] == b'/' && root.as_bytes()[1].is_ascii_alphabetic() && root.as_bytes()[2] == b':';
    let key = if windows { directory.to_lowercase() } else { directory.clone() };
    let parent = if windows { root.to_lowercase() } else { root.clone() };
    if key != parent && !key.starts_with(&format!("{parent}/")) {
        return None;
    }
    let subpath = (key != parent).then(|| directory.chars().skip(crate::js::length(&root) + 1).collect());
    Some((event_project.to_owned(), subpath))
}
