//! The patch tool (packages/core/src/tool/plugin/patch.ts): a `*** Begin Patch` envelope of file additions, deletions
//! and updates (moves included) in the format of [`super::patch_text`], prepared hunk by hunk (each target resolved and
//! its external directory approved, later hunks seeing earlier ones' results), approved as one `edit` of every file it
//! touches with the trimmed diffs as the preview, applied in order, then formatted. A failure after some files were
//! written says which.
//!
//! The session hook of the same plugin shows the tool only to OpenAI GPT models (not `gpt-4*` or `*oss*`), which lose
//! `edit` and `write` instead ([`filter`]).

use opencode_protocol::Json;

use super::file_diff::file_diff;
use super::file_write::{self, ReadError};
use super::patch_text::{self, Hunk};
use super::{Context, Definition, Execution, Failure, Output, Snapshot, Tool, ToolError};
use crate::file_access::{Kind, Target};
use crate::json::{self, Obj};
use crate::permission::Ask;

pub struct Patch {
    pub definition: Definition,
}

/// The plugin's `context`, `compaction` and `generate` hook: GPT models (not `gpt-4*` or `*oss*`) edit through the
/// patch tool, every other model through `edit` and `write`.
pub fn filter(tools: &mut Snapshot, model_id: &str) {
    let use_patch = model_id.contains("gpt-") && !model_id.contains("oss") && !model_id.contains("gpt-4");
    tools.tools.retain(|tool| {
        let name = tool.definition().name.as_str();
        if use_patch { name != "edit" && name != "write" } else { name != "patch" }
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Add,
    Delete,
    Update,
}

/// A hunk ready to apply.
struct Prepared {
    change: Change,
    target: Target,
    /// What is written (with its BOM); nothing for a deletion.
    content: String,
    before: String,
    after: String,
    move_target: Option<Target>,
}

fn failure(message: impl Into<String>) -> Failure {
    Failure::Error(ToolError::new(message))
}

/// `errorMessage` of a file system failure.
fn error_message(error: &ReadError) -> String {
    match error {
        ReadError::NotFound => "file does not exist".into(),
        ReadError::WrongKind("directory") => "path is a directory".into(),
        ReadError::WrongKind(other) => format!("path is {other}"),
        // TODO(port): the system error's own text (Node's `EACCES: permission denied, open '…'`).
        ReadError::Failed => "the file system refused the operation".into(),
    }
}

fn io_message(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "file does not exist".into(),
        std::io::ErrorKind::IsADirectory => "path is a directory".into(),
        _ => error_message(&ReadError::Failed),
    }
}

impl Patch {
    async fn run(&self, context: Context, input: Json) -> Result<Output, Failure> {
        let input = super::file_input::decode(&self.definition, &input, &[])?;
        let text = json::get_str(&input, "patchText").unwrap_or_default().to_owned();
        let parsed = patch_text::parse(&text);
        // `FileMutation.withLock` over every path the patch names, resolved lexically.
        let directory = context.location.directory.clone();
        let lock_targets: Vec<String> = match &parsed {
            Ok(hunks) => hunks
                .iter()
                .flat_map(|hunk| {
                    let moved = match hunk {
                        Hunk::Update { move_path: Some(path), .. } => Some(crate::file_access::resolve_path(&directory, path)),
                        _ => None,
                    };
                    std::iter::once(crate::file_access::resolve_path(&directory, hunk.path())).chain(moved)
                })
                .collect(),
            Err(_) => vec![],
        };
        let _lock = file_write::lock(&lock_targets).await;
        if text.is_empty() {
            return Err(failure("patchText is required"));
        }
        let hunks = parsed.map_err(|error| failure(format!("patch verification failed: {error}")))?;
        if hunks.is_empty() {
            return Err(failure("patch rejected: empty patch"));
        }
        let mut prepared: Vec<Prepared> = Vec::new();
        // What each touched file holds after the hunks so far (`None`: deleted).
        let mut updates: indexmap::IndexMap<String, Option<String>> = indexmap::IndexMap::new();
        for hunk in &hunks {
            prepare(&context, hunk, &mut prepared, &mut updates).await?;
        }
        let patch_files: Vec<Json> = prepared.iter().map(|change| patch_file(change, None)).collect();
        let mut resources: Vec<String> = Vec::new();
        for target in prepared.iter().flat_map(|change| std::iter::once(&change.target).chain(change.move_target.as_ref())) {
            if !resources.contains(&target.resource) {
                resources.push(target.resource.clone());
            }
        }
        let all_resources: Vec<String> = prepared
            .iter()
            .flat_map(|change| std::iter::once(&change.target).chain(change.move_target.as_ref()))
            .map(|target| target.resource.clone())
            .collect();
        let mut metadata = Obj::new();
        metadata.str("filepath", &all_resources.join(", "));
        metadata.str(
            "diff",
            &patch_files.iter().map(|file| format!("{}\n", json::get_str(file, "patch").unwrap_or_default())).collect::<String>(),
        );
        metadata.set("files", Json::Array(patch_files));
        let ask = Ask { action: "edit".into(), resources, save: Some(vec!["*".into()]), metadata: Some(metadata.done()) };
        crate::session::plan::assert(&context, "patch", ask).await?;

        let mut applied: Vec<(Change, String, String)> = Vec::new();
        let fail = |applied: &[(Change, String, String)], operation: String, message: String| {
            let completed = applied.iter().map(|(_, resource, _)| resource.as_str()).collect::<Vec<_>>().join(", ");
            let suffix = if completed.is_empty() { String::new() } else { format!(". Completed before failure: {completed}") };
            failure(format!("{operation}: {message}{suffix}"))
        };
        for change in &prepared {
            let target = change.move_target.as_ref().unwrap_or(&change.target);
            if change.change == Change::Delete {
                if let Err(error) = std::fs::remove_file(&change.target.absolute) {
                    return Err(fail(&applied, format!("Failed to delete {}", change.target.resource), io_message(&error)));
                }
            } else if let Err(error) = write(&target.absolute, &change.content) {
                return Err(fail(&applied, format!("Failed to write {}", target.resource), io_message(&error)));
            }
            if let Some(moved) = &change.move_target {
                if let Err(error) = std::fs::remove_file(&change.target.absolute) {
                    let operation = format!("Wrote {} but failed to remove {}", moved.resource, change.target.resource);
                    return Err(fail(&applied, operation, io_message(&error)));
                }
            }
            applied.push((change.change, target.resource.clone(), target.absolute.clone()));
        }
        let mut formatted: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for target in updates.iter().filter(|(_, value)| value.is_some()).map(|(target, _)| target.clone()) {
            let (bom, text) = file_write::read_text(&target)
                .map_err(|error| fail(&applied, format!("Failed to read {target}"), error_message(&error)))?;
            let text = if crate::formatter::file(&context.location, &target).await {
                file_write::sync_text_bom(&target, bom)
                    .map_err(|error| fail(&applied, format!("Failed to sync {target}"), error_message(&error)))?
            } else {
                text
            };
            formatted.insert(target, text);
        }
        let files: Vec<Json> = prepared
            .iter()
            .map(|change| {
                if change.change == Change::Delete {
                    return patch_file(change, None);
                }
                let target = change.move_target.as_ref().unwrap_or(&change.target);
                patch_file(change, formatted.get(&target.absolute).map(String::as_str))
            })
            .collect();
        let mut lines = vec!["Success. Updated the following files:".to_owned()];
        for (change, resource, _) in &applied {
            let letter = match change {
                Change::Add => "A",
                Change::Delete => "D",
                Change::Update => "M",
            };
            lines.push(format!("{letter} {resource}"));
        }
        let mut content = Obj::new();
        content.str("type", "text").str("text", &lines.join("\n"));
        let mut metadata = Obj::new();
        metadata.set("files", Json::Array(files));
        Ok(Output { content: vec![content.done()], metadata: Some(metadata.done()) })
    }
}

/// `environment.files.write`: the parent directories created.
fn write(path: &str, content: &str) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content.as_bytes())
}

/// `resolveTarget`: a hunk's path resolved as a file, its external directory approved.
async fn resolve_target(context: &Context, path: &str) -> Result<Target, Failure> {
    let target = crate::file_access::resolve(&context.location, path, Some(Kind::File)).await;
    let Some(external) = target.external.clone() else { return Ok(target) };
    let mut metadata = Obj::new();
    metadata.str("filepath", &target.absolute).str("parentDir", &external.directory);
    crate::file_access::authorize_external_for(context, "patch", std::slice::from_ref(&target), Some(metadata.done())).await?;
    Ok(target)
}

/// One hunk prepared against what the earlier hunks left (`toolFailure("Unable to prepare patch at …")` for any
/// failure that is not already the tool's own).
async fn prepare(
    context: &Context,
    hunk: &Hunk,
    prepared: &mut Vec<Prepared>,
    updates: &mut indexmap::IndexMap<String, Option<String>>,
) -> Result<(), Failure> {
    let target = resolve_target(context, hunk.path()).await?;
    let verification = |message: String| failure(format!("patch verification failed: {message}"));
    match hunk {
        Hunk::Add { contents, .. } => {
            let content = if contents.ends_with('\n') || contents.is_empty() { contents.clone() } else { format!("{contents}\n") };
            let after = file_write::split(&content).1.to_owned();
            updates.insert(target.absolute.clone(), Some(content.clone()));
            prepared.push(Prepared { change: Change::Add, target, content, before: String::new(), after, move_target: None });
        }
        Hunk::Delete { .. } => {
            let previous = updates.get(&target.absolute).cloned();
            let before = match previous {
                Some(None) => {
                    return Err(verification(format!("Failed to delete {}: file does not exist", target.resource)));
                }
                Some(Some(previous)) => file_write::split(&previous).1.to_owned(),
                None => file_write::read_text(&target.absolute)
                    .map_err(|error| verification(format!("Failed to delete {}: {}", target.resource, error_message(&error))))?
                    .1,
            };
            updates.insert(target.absolute.clone(), None);
            prepared.push(Prepared { change: Change::Delete, target, content: String::new(), before, after: String::new(), move_target: None });
        }
        Hunk::Update { path, move_path, chunks } => {
            let previous = updates.get(&target.absolute).cloned();
            let original = match previous {
                Some(None) => {
                    return Err(verification(format!("Failed to read file to update {}: file does not exist", target.absolute)));
                }
                Some(Some(previous)) => previous,
                None => {
                    let (bom, text) = file_write::read_text(&target.absolute).map_err(|error| {
                        verification(format!("Failed to read file to update {}: {}", target.absolute, error_message(&error)))
                    })?;
                    file_write::join(&text, bom)
                }
            };
            let before = file_write::split(&original).1.to_owned();
            let update = patch_text::derive(path, chunks, &original).map_err(verification)?;
            let moved = match move_path {
                Some(destination) => Some(resolve_target(context, destination).await?),
                None => None,
            };
            let move_target = moved.filter(|moved| moved.absolute != target.absolute);
            let next = file_write::join(&update.content, update.bom);
            match &move_target {
                Some(moved) => {
                    updates.insert(target.absolute.clone(), None);
                    updates.insert(moved.absolute.clone(), Some(next.clone()));
                }
                None => {
                    updates.insert(target.absolute.clone(), Some(next.clone()));
                }
            }
            prepared.push(Prepared { change: Change::Update, target, content: next, before, after: update.content, move_target });
        }
    }
    Ok(())
}

/// `patchFile`: the change's diff (named after the original path), shown under its final resource with the common
/// indentation trimmed.
fn patch_file(change: &Prepared, after: Option<&str>) -> Json {
    let resource = change.move_target.as_ref().unwrap_or(&change.target).resource.clone();
    let status = match change.change {
        Change::Add => "added",
        Change::Delete => "deleted",
        Change::Update => "modified",
    };
    let mut diff = file_diff(&change.target.absolute, &change.before, after.unwrap_or(&change.after), status);
    diff.file = resource;
    diff.patch = trim_diff(&diff.patch);
    diff.to_json()
}

fn is_content(line: &str) -> bool {
    (line.starts_with('+') || line.starts_with('-') || line.starts_with(' ')) && !line.starts_with("---") && !line.starts_with("+++")
}

/// `trimDiff`: the indentation every changed or context line shares, removed.
fn trim_diff(diff: &str) -> String {
    let lines: Vec<&str> = diff.split('\n').collect();
    let content: Vec<&str> = lines.iter().copied().filter(|line| is_content(line)).collect();
    if content.is_empty() {
        return diff.to_owned();
    }
    let indent = content
        .iter()
        .filter_map(|line| {
            let value = &line[1..];
            if crate::text::trim(value).is_empty() {
                return None;
            }
            // `/^(\s*)/`: JavaScript whitespace, one UTF-16 unit each.
            Some(value.chars().take_while(|c| crate::text::is_js_whitespace(*c)).count())
        })
        .min();
    let Some(indent) = indent.filter(|indent| *indent > 0) else { return diff.to_owned() };
    lines
        .iter()
        .map(|line| {
            if !is_content(line) {
                return (*line).to_owned();
            }
            let rest: String = line[1..].chars().skip(indent).collect();
            format!("{}{rest}", &line[..1])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Tool for Patch {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn permission(&self) -> &str {
        "edit"
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(self.run(context, input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_common_indentation() {
        let diff = "Index: a\n===\n--- a\n+++ a\n@@ -1,2 +1,2 @@\n     one\n-    two\n+    TWO\n";
        assert_eq!(trim_diff(diff), "Index: a\n===\n--- a\n+++ a\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n");
        assert_eq!(trim_diff("@@\n x\n-y\n"), "@@\n x\n-y\n");
    }
}
