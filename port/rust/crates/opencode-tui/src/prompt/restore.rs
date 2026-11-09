//! Putting a sent prompt back in the composer: `prompt/codec.ts` (`projectedPromptInput`, `restoredPromptInput`)
//! and `prompt/restore.ts` (`usePromptRestore`): files the server kept as `media:<hash>` refs are read back into
//! `data:` URIs, so the draft owns its bytes after the server releases the blob.
use base64::Engine;
use opencode_protocol::Json;
use opencode_tui_sync::client::Error;
use opencode_tui_sync::context::client::{is_missing_media, media_hash_from_uri};
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::runtime::Deferred;

use crate::ctx::Tui;
use crate::prompt::history::{PromptAgent, PromptFile, PromptInfo, PromptSkill, Range};
use crate::toast::{ToastInput, Variant};

fn mention(value: Option<&Json>) -> Option<Range> {
    let value = value?;
    Some(Range {
        start: value.get("start").and_then(num).unwrap_or(0.0) as i64,
        end: value.get("end").and_then(num).unwrap_or(0.0) as i64,
        text: value.str("text").unwrap_or_default().to_string(),
    })
}

fn num(value: &Json) -> Option<f64> {
    match value {
        Json::Number(n) => Some(*n),
        _ => None,
    }
}

/// `projectedPromptInput(input)`: a projected prompt (`text`, `files`, `agents`, `skills`) as composer input.
pub fn projected_prompt_input(input: &Json) -> PromptInfo {
    let files = input.arr("files").map(|files| {
        files
            .iter()
            .map(|file| {
                let source = file.get("source");
                let uri = if source.and_then(|s| s.str("type")) == Some("uri") {
                    source.and_then(|s| s.str("uri")).unwrap_or_default().to_string()
                } else if let Some(uri) = file.str("uri") {
                    uri.to_string()
                } else {
                    format!("data:{};base64,{}", file.str("mime").unwrap_or_default(), file.str("data").unwrap_or_default())
                };
                PromptFile {
                    uri,
                    name: file.str("name").map(str::to_string),
                    description: file.str("description").map(str::to_string),
                    path: None,
                    mention: mention(file.get("mention")),
                }
            })
            .collect()
    });
    let agents = input.arr("agents").map(|agents| {
        agents
            .iter()
            .map(|agent| PromptAgent { name: agent.str("name").unwrap_or_default().to_string(), mention: mention(agent.get("mention")) })
            .collect()
    });
    let skills = input.arr("skills").map(|skills| {
        skills
            .iter()
            .map(|skill| PromptSkill { id: skill.str("id").unwrap_or_default().to_string(), mention: mention(skill.get("mention")) })
            .collect()
    });
    PromptInfo { text: input.str("text").unwrap_or_default().to_string(), files, agents, skills, pasted: vec![], mode: None }
}

/// `usePromptRestore()(input, { allowMissing })`: the prompt with its images as bytes. An image the server no longer
/// has is left out with a warning unless `allow_missing` is false (then the read rejects); any other failed read
/// rejects.
pub fn restore_prompt(t: &Tui, input: &Json, allow_missing: bool) -> Deferred<Result<PromptInfo, Error>> {
    let mut output = projected_prompt_input(input);
    let client = t.data.client_context.clone();
    let toast = t.toast.clone();
    let mimes: Vec<Option<String>> =
        input.arr("files").map(|f| f.iter().map(|file| file.str("mime").map(str::to_string)).collect()).unwrap_or_default();
    Deferred::spawn(async move {
        let Some(files) = output.files.take() else { return Ok(PromptInfo { pasted: vec![], ..output }) };
        if !files.iter().any(|file| media_hash_from_uri(&file.uri).is_some()) {
            output.files = Some(files);
            return Ok(output);
        }
        let mut missing = 0usize;
        let mut restored = Vec::with_capacity(files.len());
        for (index, file) in files.into_iter().enumerate() {
            if media_hash_from_uri(&file.uri).is_none() {
                restored.push(file);
                continue;
            }
            let read = client.media().read(&file.uri);
            match read.wait().await {
                Ok(bytes) => {
                    let mime = mimes.get(index).cloned().flatten().unwrap_or_else(|| "application/octet-stream".to_string());
                    let data = base64::engine::general_purpose::STANDARD.encode(bytes.as_slice());
                    restored.push(PromptFile { uri: format!("data:{mime};base64,{data}"), ..file });
                }
                Err(error) => {
                    if !allow_missing || !is_missing_media(&error) {
                        return Err(error);
                    }
                    missing += 1;
                }
            }
        }
        if missing > 0 {
            let message = if missing == 1 {
                "An image in this message is no longer stored, so it can't be restored".to_string()
            } else {
                format!("{missing} images in this message are no longer stored, so they can't be restored")
            };
            toast.show(ToastInput::new(Variant::Warning, message));
        }
        output.files = Some(restored);
        Ok(output)
    })
}
