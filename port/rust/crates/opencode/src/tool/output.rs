//! Tool output bounding (packages/core/src/tool-output.ts `ToolOutput.truncate`, config/plugin/tool-output.ts): every
//! local tool result passes through it before it is recorded. Text beyond the limits (`tool_output.max_lines` and
//! `max_bytes` of the location's config, else 2,000 lines and 50 KiB) is cut at a line boundary, the full text is saved
//! under `<data>/tool-output/tool_<id>`, and a marker naming the file replaces the rest; the result's metadata records
//! `truncated` (and `outputPath`). A result whose metadata already says `truncated` passes unchanged.
//!
//! Saved outputs older than seven days are removed when the first location boots and every hour after
//! (`tool-output-cleanup`, `FileRetention.cleanup`). TS has no clock to inject here: its own tests call `cleanup`
//! directly; what a client observes is the sweep at startup.

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

/// `ToolOutput.truncate` under the location's limits (`ToolOutput.configure`).
pub fn bound(output: super::Output, location: &crate::location::Location) -> super::Output {
    let (lines, bytes) = crate::session::shell::output_limits(&location.inventory());
    truncate(output, lines as usize, bytes as usize)
}

/// `ToolOutput.truncate` with `max_lines` and `max_bytes`.
pub fn truncate(output: super::Output, max_lines: usize, max_bytes: usize) -> super::Output {
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
    if lines.len() <= max_lines && text.len() <= max_bytes {
        let metadata = with_metadata(&output.metadata, &[("truncated", Json::Bool(false))]);
        return super::Output { content: output.content, metadata: Some(metadata) };
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0;
    for line in lines.iter().take(max_lines) {
        let size = line.len() + usize::from(!kept.is_empty());
        if bytes + size > max_bytes {
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

/// How long saved outputs are kept (`ToolOutput.RETENTION`).
const RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// `ToolOutput.cleanup`: removes saved outputs (`tool_<12 hex>…`) whose modification time is older than the retention.
/// Entries that cannot be read or removed (a directory) stay.
fn cleanup() {
    let dir = directory();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let cutoff = std::time::SystemTime::now() - RETENTION;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let saved = name.len() >= 17
            && name.starts_with("tool_")
            && name.as_bytes()[5..17].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b));
        if !saved {
            continue;
        }
        // `fs.stat` follows links; `remove` does not recurse.
        let path = entry.path();
        if std::fs::metadata(&path).and_then(|m| m.modified()).is_ok_and(|mtime| mtime < cutoff) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Starts the hourly sweep of saved outputs once per process (the global `tool-output-cleanup` node), the first run
/// right away.
pub fn start_cleanup() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        tokio::spawn(async {
            loop {
                let _ = tokio::task::spawn_blocking(cleanup).await;
                tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
            }
        });
    });
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
        let out = truncate(super::super::Output { content: vec![text("a\nb\n")], metadata: None }, MAX_LINES, MAX_BYTES);
        assert_eq!(json::text(&out.metadata.unwrap()), r#"{"truncated":false}"#);
        let kept = truncate(super::super::Output { content: vec![text("x")], metadata: json::parse(r#"{"truncated":true}"#) }, 1, 1);
        assert_eq!(json::text(&kept.metadata.unwrap()), r#"{"truncated":true}"#);
    }
}
