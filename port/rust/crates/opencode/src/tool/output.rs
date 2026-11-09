//! Tool output bounding (packages/core/src/tool-output.ts `ToolOutput.truncate`): every local tool result passes
//! through it before it is recorded. Text beyond 2,000 lines or 50 KiB is cut at a line boundary, the full text is
//! saved under `<data>/tool-output/tool_<id>`, and a marker naming the file replaces the rest; the result's metadata
//! records `truncated` (and `outputPath`). A result whose metadata already says `truncated` passes unchanged.
//!
//! TODO(port): the configurable limits (`ToolOutput.configure`) and the hourly cleanup of saved outputs.

use opencode_protocol::Json;

use crate::json::{self, Obj};

pub const MAX_LINES: usize = 2_000;
pub const MAX_BYTES: usize = 50 * 1024;

fn directory() -> String {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("{}/.local/share", std::env::var("HOME").unwrap_or_default()));
    format!("{base}/opencode/tool-output")
}

fn with_metadata(metadata: &Option<Json>, entries: &[(&str, Json)]) -> Json {
    let mut out = metadata.clone().filter(|m| matches!(m, Json::Object(_))).unwrap_or_else(json::object);
    for (key, value) in entries {
        json::set(&mut out, key, value.clone());
    }
    out
}

fn utf16_prefix(text: &str, units: usize) -> String {
    let encoded: Vec<u16> = text.encode_utf16().collect();
    String::from_utf16_lossy(&encoded[..units.min(encoded.len())])
}

/// `ToolOutput.truncate`.
pub fn truncate(output: super::Output) -> super::Output {
    if output.metadata.as_ref().and_then(|m| json::get(m, "truncated")).is_some() {
        return output;
    }
    let text: String = output
        .content
        .iter()
        .filter(|item| json::get_str(item, "type") == Some("text"))
        .filter_map(|item| json::get_str(item, "text"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    if lines.len() <= MAX_LINES && text.len() <= MAX_BYTES {
        let metadata = with_metadata(&output.metadata, &[("truncated", Json::Bool(false))]);
        return super::Output { content: output.content, metadata: Some(metadata) };
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0;
    for line in lines.iter().take(MAX_LINES) {
        let size = line.len() + usize::from(!kept.is_empty());
        if bytes + size > MAX_BYTES {
            break;
        }
        kept.push(line);
        bytes += size;
    }
    let dir = directory();
    let file = format!("{dir}/tool_{}", crate::id::ascending());
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&file, &text);
    let shown = if kept.is_empty() { "0 lines".to_owned() } else { format!("lines 1-{}", kept.len()) };
    let marker = format!("[showing {shown} of {}; full output saved to {file}]", lines.len());
    let marker_item = || {
        let mut o = Obj::new();
        o.str("type", "text").str("text", &marker);
        o.done()
    };
    let mut bounded = Vec::new();
    let mut remaining = kept.join("\n").encode_utf16().count();
    let mut seen_text = false;
    let mut marked = false;
    for item in &output.content {
        if json::get_str(item, "type") == Some("file") {
            bounded.push(item.clone());
            continue;
        }
        if seen_text && remaining > 0 {
            remaining -= 1;
        }
        seen_text = true;
        let item_text = json::get_str(item, "text").unwrap_or_default();
        let length = item_text.encode_utf16().count();
        if remaining >= length {
            bounded.push(item.clone());
            remaining -= length;
            continue;
        }
        if remaining > 0 {
            let mut cut = item.clone();
            json::set(&mut cut, "text", json::string(&utf16_prefix(item_text, remaining)));
            bounded.push(cut);
        }
        if !marked {
            bounded.push(marker_item());
        }
        remaining = 0;
        marked = true;
    }
    if !marked {
        bounded.push(marker_item());
    }
    let metadata = with_metadata(&output.metadata, &[("truncated", Json::Bool(true)), ("outputPath", json::string(&file))]);
    super::Output { content: bounded, metadata: Some(metadata) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Json {
        let mut o = Obj::new();
        o.str("type", "text").str("text", value);
        o.done()
    }

    #[test]
    fn small_output_is_marked_untruncated() {
        let out = truncate(super::super::Output { content: vec![text("a\nb\n")], metadata: None });
        assert_eq!(json::text(&out.metadata.unwrap()), r#"{"truncated":false}"#);
        let kept = truncate(super::super::Output { content: vec![text("x")], metadata: json::parse(r#"{"truncated":true}"#) });
        assert_eq!(json::text(&kept.metadata.unwrap()), r#"{"truncated":true}"#);
    }
}
