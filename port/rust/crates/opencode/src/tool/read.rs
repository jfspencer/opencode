//! The read tool (packages/core/src/tool/plugin/read.ts): a file or directory of the location (or, approved, outside
//! it), or a skill's virtual file (`skill://<id>/<file>`). A name that misses only by Unicode normalization, a
//! non-breaking space or a curly apostrophe reads the one file it means; otherwise a miss suggests similar names, or
//! names the first missing folder. A successful read of the location loads the `AGENTS.md` files next to it
//! ([`super::file_instructions`]).

use opencode_protocol::Json;
use unicode_normalization::UnicodeNormalization;

use super::file_access::{self, Target, dirname, join};
use super::file_read::{self, Content, Files, Page, ReadError};
use super::{Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::json::{self, Obj};
use crate::permission::Ask;

pub struct Read {
    pub definition: Definition,
}

fn failure(message: String) -> Failure {
    Failure::Error(ToolError::new(message))
}

/// A read error as the tool reports it: the reader's own errors with their messages (their session error is
/// `unknown`, as `toSessionError` unwraps them), anything else as "Unable to read".
fn read_failure(error: ReadError, path: &str) -> Failure {
    match error {
        ReadError::Message(message) => Failure::Error(ToolError { kind: "unknown", message, metadata: None }),
        _ => failure(format!("Unable to read {path}")),
    }
}

async fn read_local(target: &Target, page: Page) -> Result<Content, ReadError> {
    let (absolute, resource) = (target.absolute.clone(), target.resource.clone());
    tokio::task::spawn_blocking(move || file_read::read(&file_read::Local, &absolute, &resource, page))
        .await
        .unwrap_or(Err(ReadError::Failed))
}

/// `reader.list` of the session's environment.
fn list(path: &str) -> Result<Vec<(String, file_read::FileType)>, file_read::FsError> {
    file_read::Local.list(path)
}

fn canonical(name: &str) -> String {
    name.nfc().map(|c| if matches!(c, '\u{a0}' | '\u{202f}') { ' ' } else if matches!(c, '\u{2018}' | '\u{2019}') { '\'' } else { c }).collect()
}

/// `alternatePath`: the one file of the directory whose name matches when normalized.
fn alternate_path(absolute: &str) -> Option<String> {
    let base = canonical(crate::text::basename(absolute));
    let directory = dirname(absolute);
    let entries = list(&directory).ok()?;
    let matches: Vec<&String> =
        entries.iter().filter(|(name, kind)| *kind == file_read::FileType::File && canonical(name) == base).map(|(name, _)| name).collect();
    if matches.len() != 1 {
        return None;
    }
    Some(join(&directory, matches[0]))
}

/// `similar`: up to three names containing the wanted one or contained in it, ignoring case.
fn similar(names: &[String], name: &str) -> Vec<String> {
    let base = name.to_lowercase();
    names
        .iter()
        .filter(|entry| {
            let candidate = entry.to_lowercase();
            candidate.contains(&base) || base.contains(&candidate)
        })
        .take(3)
        .cloned()
        .collect()
}

enum Listing {
    Found(Vec<String>),
    Missing,
    Unknown,
}

fn listing(path: &str) -> Listing {
    match list(path) {
        Ok(entries) => Listing::Found(entries.into_iter().map(|(name, _)| name).collect()),
        Err(file_read::FsError::NotFound) => Listing::Missing,
        Err(_) => Listing::Unknown,
    }
}

/// `path.join(a, b)` for a possibly relative `a`.
fn join_relative(a: &str, b: &str) -> String {
    crate::project::normalize(&format!("{a}/{b}"))
}

/// `missingFolder`: the first missing folder and its nearest existing parent.
fn missing_folder(input: &str, folder: &str) -> Failure {
    let mut child = folder.to_owned();
    let mut parent = dirname(folder);
    while parent != child {
        match listing(&parent) {
            Listing::Unknown => break,
            Listing::Found(names) => {
                let suggestions: Vec<String> =
                    similar(&names, crate::text::basename(&child)).iter().map(|entry| join(&parent, entry)).collect();
                let mut parts = vec![
                    format!("File not found: {input}"),
                    format!("The folder {child} does not exist. The nearest folder that does is {parent}."),
                ];
                if !suggestions.is_empty() {
                    parts.push(format!("Did you mean one of these?\n{}", suggestions.join("\n")));
                }
                return failure(parts.join("\n\n"));
            }
            Listing::Missing => {
                child = parent.clone();
                parent = dirname(&parent);
            }
        }
    }
    failure(format!("File not found: {input}"))
}

/// `missing`: names similar to the wanted one in its folder, or the folder that is missing.
fn missing(input: &str, absolute: &str) -> Failure {
    let folder = dirname(absolute);
    let suggestions = match listing(&folder) {
        Listing::Missing => return missing_folder(input, &folder),
        Listing::Found(names) => {
            similar(&names, crate::text::basename(input)).iter().map(|entry| join_relative(&dirname(input), entry)).collect()
        }
        Listing::Unknown => vec![],
    };
    if suggestions.is_empty() {
        failure(format!("File not found: {input}"))
    } else {
        failure(format!("File not found: {input}\n\nDid you mean one of these?\n{}", suggestions.join("\n")))
    }
}

/// `parseSkillUri`.
fn parse_skill_uri(value: &str) -> Option<(String, String, String)> {
    let trimmed = value["skill://".len()..].trim_end_matches('/');
    let (id, rest) = trimmed.split_once('/').unwrap_or((trimmed, ""));
    let safe = |segment: &str| !segment.is_empty() && segment != "." && segment != ".." && !segment.contains(['/', '\\', '\0']);
    if !safe(id) || (!rest.is_empty() && !rest.split('/').all(safe)) {
        return None;
    }
    let target = if rest.is_empty() { "/".to_owned() } else { format!("/{rest}") };
    let uri = crate::session::skill::uri(id, (!rest.is_empty()).then_some(rest));
    Some((id.to_owned(), target, uri))
}

async fn read_skill(context: &Context, path: &str, page: Page) -> Result<Content, Failure> {
    let not_found = || failure(format!("File not found: {path}"));
    let (id, target, uri) = parse_skill_uri(path).ok_or_else(not_found)?;
    let skill = context.location.inventory().skill(&id).cloned().ok_or_else(not_found)?;
    if !skill.path.0.starts_with("skill://") {
        return Err(not_found());
    }
    context
        .assert(Ask { action: "skill".into(), resources: vec![skill.id.0.clone()], save: Some(vec![skill.id.0.clone()]), metadata: None })
        .await?;
    context.assert(Ask { action: "read".into(), resources: vec![uri], save: Some(vec!["*".into()]), metadata: None }).await?;
    // TODO(port): the virtual files plugins add to a skill (`Skill.files`); built-in skills have none.
    let files = file_read::Memory { files: vec![("/SKILL.md".into(), skill.content.into_bytes())] };
    match file_read::read(&files, &target, path, page) {
        Ok(content) => Ok(content),
        Err(ReadError::NotFound) => Err(not_found()),
        Err(error) => Err(read_failure(error, path)),
    }
}

/// `toModelContent`.
fn model_content(path: &str, offset: Option<usize>, output: &Content) -> Vec<Json> {
    let text = |text: String| {
        let mut o = Obj::new();
        o.str("type", "text").str("text", &text);
        o.done()
    };
    match output {
        Content::File { content, base64: true, mime } => {
            let mut file = Obj::new();
            file.str("type", "file").str("uri", &format!("data:{mime};base64,{content}")).str("mime", mime).str("name", path);
            let note = if mime == "application/pdf" { "PDF read successfully" } else { "Image read successfully" };
            vec![text(note.into()), file.done()]
        }
        Content::ListPage { entries, next } => {
            let start = offset.filter(|o| *o > 0).unwrap_or(1);
            let mut lines = vec![if entries.is_empty() {
                format!("Read directory {path}, 0 entries")
            } else {
                format!("Read directory {path}, entries {start}-{}", start + entries.len() - 1)
            }];
            lines.extend(entries.iter().map(|entry| entry.path.clone()));
            if let Some(next) = next {
                lines.push(format!("[Output truncated. Continue reading with offset: {next}]"));
            }
            vec![text(lines.join("\n"))]
        }
        Content::File { content, .. } | Content::TextPage { content, .. } => {
            let (start, next) = match output {
                Content::TextPage { offset, next, .. } => (*offset, *next),
                _ => (1, None),
            };
            let body = match output {
                Content::File { .. } => content.strip_suffix('\n').unwrap_or(content),
                _ => content.as_str(),
            };
            let lines: Vec<&str> = if content.is_empty() { vec![] } else { body.split('\n').collect() };
            let mut out = vec![if lines.is_empty() {
                format!("Read file {path}, 0 lines")
            } else {
                format!("Read file {path}, lines {start}-{}", start + lines.len() - 1)
            }];
            out.extend(lines.iter().enumerate().map(|(index, line)| format!("{}: {line}", start + index)));
            if let Some(next) = next {
                out.push(format!("[Output truncated. Continue reading with offset: {next}]"));
            }
            vec![text(out.join("\n"))]
        }
    }
}

impl Read {
    async fn run(&self, context: Context, input: Json) -> Result<Output, Failure> {
        let input = super::file_input::decode(&self.definition, &input, &[])?;
        let path = json::get_str(&input, "path").unwrap_or_default().to_owned();
        let number = |key: &str| json::get_num(&input, key).map(|n| n as usize);
        let page = Page { offset: number("offset"), limit: number("limit") };
        let (content, shown) = if path.starts_with("skill://") {
            (read_skill(&context, &path, page).await?, path.clone())
        } else {
            let requested = file_access::authorize_read(&context, &path, None).await?;
            let (content, target, shown) = match read_local(&requested, page).await {
                Ok(content) => (content, requested, path.clone()),
                Err(ReadError::NotFound) => {
                    let Some(alternate) = alternate_path(&requested.absolute) else {
                        return Err(missing(&path, &requested.absolute));
                    };
                    let target = file_access::authorize_read(&context, &alternate, Some(&requested)).await?;
                    let content = match read_local(&target, page).await {
                        Err(ReadError::NotFound) => return Err(missing(&path, &requested.absolute)),
                        Err(error) => return Err(read_failure(error, &path)),
                        Ok(Content::ListPage { .. }) => return Err(missing(&path, &requested.absolute)),
                        Ok(content) => content,
                    };
                    let shown = join_relative(&dirname(&path), crate::text::basename(&alternate));
                    (content, target, shown)
                }
                Err(error) => return Err(read_failure(error, &path)),
            };
            if target.external.is_none() {
                super::file_instructions::discover(&context, &target.absolute, matches!(content, Content::ListPage { .. }));
            }
            (content, shown)
        };
        let mut metadata = Obj::new();
        metadata.set("truncated", Json::Bool(content.truncated()));
        Ok(Output { content: model_content(&shown, page.offset, &content), metadata: Some(metadata.done()) })
    }
}

impl Tool for Read {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(self.run(context, input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_uris() {
        assert_eq!(parse_skill_uri("skill://opencode/").map(|p| p.1), Some("/".into()));
        assert_eq!(parse_skill_uri("skill://opencode/SKILL.md").map(|p| p.2), Some("skill://opencode/SKILL.md".into()));
        assert!(parse_skill_uri("skill://../etc/passwd").is_none());
        assert!(parse_skill_uri("skill://opencode/../report/SKILL.md").is_none());
    }

    #[test]
    fn canonical_names() {
        assert_eq!(canonical("it\u{2019}s here.md"), canonical("it's here.md"));
        assert_eq!(canonical("non\u{a0}breaking.txt"), "non breaking.txt");
        assert_eq!(canonical("cafe\u{301}.txt"), canonical("caf\u{e9}.txt"));
    }
}
