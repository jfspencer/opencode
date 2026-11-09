//! `session/load`'s replay (packages/cli/src/acp/replay.ts): the session's messages, oldest first, as the updates a
//! client would have seen live.

use opencode_protocol::Json;

use super::translate::{DiffSource, compaction_replay, reasoning_message_id};
use super::{Agent, RpcError, classify, tool};
use crate::json::{self, Obj, get, get_str};

/// `ACPReplay.history`.
pub async fn history(agent: &Agent, session: &str, cwd: &str, compaction: bool) -> Result<(), RpcError> {
    let mut cursor: Option<String> = None;
    loop {
        let mut query = Obj::new();
        query.int("limit", 200);
        match &cursor {
            Some(cursor) => {
                query.str("cursor", cursor);
            }
            None => {
                query.str("order", "asc");
            }
        }
        let page = agent.client.call("session.message.list", &[("sessionID", session)], Some(query.done()), None).await.map_err(classify)?;
        for message in get(&page, "data").and_then(json::array).unwrap_or(&[]) {
            for update in updates(message, cwd, compaction) {
                let source = completed_source(message, &update);
                let update = super::permission::with_completed_diffs(update, source.as_ref(), cwd).await;
                agent.conn.update(session, update);
            }
        }
        match get(&page, "cursor").and_then(|c| get_str(c, "next")) {
            Some(next) => cursor = Some(next.to_owned()),
            None => return Ok(()),
        }
    }
}

/// `ACPReplay.updates`.
fn updates(message: &Json, cwd: &str, compaction: bool) -> Vec<Json> {
    let id = get_str(message, "id").unwrap_or("");
    match get_str(message, "type") {
        Some("user") => {
            let mut content = Obj::new();
            content.str("type", "text").str("text", get_str(message, "text").unwrap_or(""));
            let mut first = Obj::new();
            first.str("sessionUpdate", "user_message_chunk").str("messageId", id).set("content", content.done());
            let files: Vec<(String, Option<String>, String)> = get(message, "files")
                .and_then(json::array)
                .unwrap_or(&[])
                .iter()
                .map(|file| {
                    let mime = get_str(file, "mime").unwrap_or("").to_owned();
                    let source = get(file, "source");
                    let url = match source.and_then(|s| get_str(s, "type")) {
                        Some("uri") => source.and_then(|s| get_str(s, "uri")).unwrap_or("").to_owned(),
                        _ => format!("data:{mime};base64,{}", get_str(file, "data").unwrap_or("")),
                    };
                    (url, get_str(file, "name").map(String::from), mime)
                })
                .collect();
            std::iter::once(first.done())
                .chain(super::prompt::file_chunks(&files).into_iter().map(|chunk| {
                    let mut o = Obj::new();
                    o.str("sessionUpdate", "user_message_chunk").str("messageId", id).opt("content", get(&chunk, "content").cloned());
                    o.done()
                }))
                .collect()
        }
        Some("compaction") => compaction_replay(message, compaction).into_iter().collect(),
        Some("assistant") => {
            let content = get(message, "content").and_then(json::array).unwrap_or(&[]);
            let mut reasoning = 0;
            let mut out = vec![];
            for part in content {
                match get_str(part, "type") {
                    Some("text") => {
                        let mut text = Obj::new();
                        text.str("type", "text").str("text", get_str(part, "text").unwrap_or(""));
                        let mut o = Obj::new();
                        o.str("sessionUpdate", "agent_message_chunk").str("messageId", id).set("content", text.done());
                        out.push(o.done());
                    }
                    Some("reasoning") => {
                        let mut text = Obj::new();
                        text.str("type", "text").str("text", get_str(part, "text").unwrap_or(""));
                        let mut o = Obj::new();
                        o.str("sessionUpdate", "agent_thought_chunk")
                            .str("messageId", &reasoning_message_id(id, &Json::Number(reasoning as f64)))
                            .set("content", text.done());
                        reasoning += 1;
                        out.push(o.done());
                    }
                    Some("tool") => out.extend(tool_updates(part, cwd)),
                    _ => {}
                }
            }
            out
        }
        _ => vec![],
    }
}

fn tool_updates(part: &Json, cwd: &str) -> Vec<Json> {
    let id = get_str(part, "id").unwrap_or("");
    let name = get_str(part, "name").unwrap_or("");
    let state = get(part, "state").cloned().unwrap_or(Json::Null);
    let status = get_str(&state, "status").unwrap_or("");
    let input = if status == "streaming" { json::object() } else { get(&state, "input").cloned().unwrap_or_else(json::object) };
    let mut call = tool::pending(id, name, &input, None, cwd);
    call.str("sessionUpdate", "tool_call");
    let content = get(&state, "content").and_then(json::array).unwrap_or(&[]);
    let metadata = get(&state, "metadata");
    let mut update = match status {
        "completed" => tool::completed(id, name, &input, content, metadata, cwd),
        "running" => tool::running(id, name, &input, cwd),
        "error" => {
            let error = get(&state, "error").and_then(|e| get_str(e, "message")).unwrap_or("");
            tool::failed(id, name, &input, content, metadata, error, cwd)
        }
        _ => return vec![call.done()],
    };
    update.str("sessionUpdate", "tool_call_update");
    vec![call.done(), update.done()]
}

/// `completedSource`: the completed tool call an update replays, for its diffs.
fn completed_source(message: &Json, update: &Json) -> Option<DiffSource> {
    if get_str(message, "type") != Some("assistant") || get_str(update, "sessionUpdate") != Some("tool_call_update") {
        return None;
    }
    let call = get_str(update, "toolCallId")?;
    let part = get(message, "content")
        .and_then(json::array)?
        .iter()
        .find(|p| get_str(p, "type") == Some("tool") && get_str(p, "id") == Some(call))?;
    let state = get(part, "state")?;
    if get_str(state, "status") != Some("completed") {
        return None;
    }
    Some(DiffSource {
        tool_name: get_str(part, "name").unwrap_or("").to_owned(),
        input: get(state, "input").cloned().unwrap_or_else(json::object),
        metadata: get(state, "metadata").cloned(),
    })
}
