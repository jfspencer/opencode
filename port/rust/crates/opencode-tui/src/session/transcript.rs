//! `formatSessionTranscript` (routes/session/index.tsx): a session as Markdown, for the copy and export commands.
use opencode_protocol::Json;
use opencode_tui_sync::json::JsonExt;

/// `new Date(at).toLocaleString()` in en-US as Bun writes it ("1/5/2026, 10:00:07 AM").
fn locale_string(at: f64) -> String {
    let Some(date) = crate::usage_limits::local_time(at) else { return "Invalid Date".into() };
    let hour = match date.hours % 12 {
        0 => 12,
        h => h,
    };
    format!(
        "{}/{}/{}, {hour}:{:02}:{:02} {}",
        date.month + 1,
        date.day,
        date.year,
        date.minutes,
        date.seconds,
        if date.hours < 12 { "AM" } else { "PM" }
    )
}

/// A template literal's `${value}`: strings as they are, `undefined` for a missing value.
fn template(value: Option<&Json>) -> String {
    value.map(opencode_tui_sync::js::string).unwrap_or_else(|| "undefined".into())
}

fn tool_entry(item: &Json, tools: bool) -> Vec<String> {
    match item.str("type") {
        Some("text") => return vec![template(item.get("text"))],
        Some("reasoning") => return Vec::new(),
        _ => {}
    }
    if !tools {
        return Vec::new();
    }
    let state = item.get("state");
    let input = match state.and_then(|s| s.get("input")) {
        Some(Json::String(text)) => text.clone(),
        Some(value) => crate::tools::display::stringify_pretty(value),
        None => "undefined".into(),
    };
    let output = match state.and_then(|s| s.str("status")) {
        Some("error") => template(state.and_then(|s| s.get("error")).and_then(|e| e.get("message"))),
        Some("streaming") => String::new(),
        _ => crate::tools::display::tool_display_content(state)
            .iter()
            .map(|entry| {
                if entry.str("type") == Some("text") {
                    template(entry.get("text"))
                } else {
                    template(entry.get("name").filter(|n| !matches!(n, Json::Null)).or_else(|| entry.get("uri")))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    vec![format!("**Tool: {}**\n\n**Input:**\n```json\n{input}\n```\n\n{output}", template(item.get("name")))]
}

/// `formatSessionTranscript(session, messages, thinking, tools)`.
pub fn format_session_transcript(session: &Json, messages: &[Json], thinking: bool, tools: bool) -> String {
    let body: Vec<String> = messages
        .iter()
        .flat_map(|message| -> Vec<String> {
            match message.str("type") {
                Some("user") => return vec![format!("## User\n\n{}", template(message.get("text")))],
                Some("shell") => {
                    let output = message.get("output").and_then(|o| o.get("output")).filter(|o| !matches!(o, Json::Null));
                    return vec![format!(
                        "## Shell\n\n```\n$ {}\n{}\n```",
                        template(message.get("command")),
                        output.map(opencode_tui_sync::js::string).unwrap_or_default()
                    )];
                }
                Some("assistant") => {}
                _ => return Vec::new(),
            }
            let content: Vec<String> = message
                .arr("content")
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .flat_map(|item| {
                    if item.str("type") == Some("reasoning") {
                        return if thinking { vec![format!("_Thinking:_\n\n{}", template(item.get("text")))] } else { Vec::new() };
                    }
                    tool_entry(item, tools)
                })
                .collect();
            if content.is_empty() {
                return Vec::new();
            }
            vec![format!("## Assistant\n\n{}", content.join("\n\n"))]
        })
        .collect();
    let time = session.get("time");
    format!(
        "# {}\n\n**Session ID:** {}\n**Created:** {}\n**Updated:** {}\n\n---\n\n{}\n",
        crate::subagents::with_timestamped_fallback(session),
        template(session.get("id")),
        locale_string(time.and_then(|t| t.num("created")).unwrap_or(f64::NAN)),
        locale_string(time.and_then(|t| t.num("updated")).unwrap_or(f64::NAN)),
        body.join("\n\n---\n\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Json {
        opencode_protocol::wire::json::parse(text).unwrap()
    }

    #[test]
    fn transcript_reads_as_markdown() {
        let session = json(r#"{"id":"ses_1","title":"Demo","time":{"created":0,"updated":0}}"#);
        let messages = [
            json(r#"{"type":"user","text":"Hi"}"#),
            json(r#"{"type":"shell","command":"ls","output":{"output":"a.txt"}}"#),
            json(
                r#"{"type":"assistant","content":[{"type":"reasoning","text":"Plan"},{"type":"text","text":"Done."},
                {"type":"tool","name":"read","state":{"status":"completed","input":{"path":"a.txt"},"content":[{"type":"text","text":"x"},{"type":"file","uri":"file:///a"}]}},
                {"type":"tool","name":"bash","state":{"status":"error","input":"ls -l","error":{"message":"boom"}}}]}"#,
            ),
            json(r#"{"type":"compaction"}"#),
        ];
        let text = format_session_transcript(&session, &messages, false, true);
        let body = text.split_once("\n\n---\n\n").unwrap().1;
        assert_eq!(
            body,
            "## User\n\nHi\n\n---\n\n## Shell\n\n```\n$ ls\na.txt\n```\n\n---\n\n## Assistant\n\nDone.\n\n**Tool: read**\n\n**Input:**\n```json\n{\n  \"path\": \"a.txt\"\n}\n```\n\nx\nfile:///a\n\n**Tool: bash**\n\n**Input:**\n```json\nls -l\n```\n\nboom\n"
        );
        assert!(text.starts_with("# Demo\n\n**Session ID:** ses_1\n**Created:** "));
        let thinking = format_session_transcript(&session, &messages[2..3], true, false);
        assert!(thinking.ends_with("## Assistant\n\n_Thinking:_\n\nPlan\n\nDone.\n"));
    }
}
