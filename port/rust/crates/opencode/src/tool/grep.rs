//! The grep tool (packages/core/src/tool/plugin/grep.ts): lines matching a ripgrep pattern (or literal text) in a
//! file or under a directory of the location, grouped by file.

use opencode_protocol::Json;

use super::file_input::Wording;
use crate::file_access::dirname;
use super::file_search::{self, DEFAULT_SEARCH_LIMIT, GrepInput, Match, SearchError};
use super::{Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::json::{self, Obj};
use crate::permission::Ask;

pub struct Grep {
    pub definition: Definition,
}

/// `toModelContent`.
fn model_content(matches: &[Match], truncated: bool) -> String {
    let mut lines = vec![if matches.is_empty() { "No matches found".to_owned() } else { format!("Found {} matches", matches.len()) }];
    let mut current = "";
    for item in matches {
        if current != item.path {
            if !current.is_empty() {
                lines.push(String::new());
            }
            current = &item.path;
            lines.push(format!("{}:", item.path));
        }
        // ripgrep's line text ends with the line's terminator; the listing puts one line per match (D120).
        let text = item.text.strip_suffix('\n').map(|text| text.strip_suffix('\r').unwrap_or(text)).unwrap_or(&item.text);
        lines.push(format!("  Line {}: {}", item.line, text));
    }
    if truncated {
        lines.push(String::new());
        lines.push(format!(
            "(Results are truncated: showing first {} results. Consider using a more specific path or pattern.)",
            matches.len()
        ));
    }
    lines.join("\n")
}

impl Grep {
    async fn run(&self, context: Context, input: Json) -> Result<Output, Failure> {
        let wording = [("pattern", Wording::Message("Pattern must not be empty")), ("path", Wording::Expected("RelativePath"))];
        let input = super::file_input::decode(&self.definition, &input, &wording)?;
        let get = |key: &str| json::get_str(&input, key).map(str::to_owned);
        let pattern = get("pattern").unwrap_or_default();
        let path = get("path");
        let include = get("include");
        let literal = json::get_bool(&input, "literal");
        let case_sensitive = json::get_bool(&input, "caseSensitive");
        let limit = json::get_num(&input, "limit");
        let display = path.clone().unwrap_or_else(|| ".".into());
        let failure = |message: String| Failure::Error(ToolError::new(message));
        let target = crate::file_access::resolve(&context.location, &display, None).await;
        crate::file_access::authorize_external(&context, std::slice::from_ref(&target), None).await?;
        let metadata = file_search::metadata(vec![
            ("root", Some(json::string("."))),
            ("path", path.as_deref().map(json::string)),
            ("include", include.as_deref().map(json::string)),
            ("literal", literal.map(Json::Bool)),
            ("caseSensitive", case_sensitive.map(Json::Bool)),
            ("limit", limit.map(Json::Number)),
        ]);
        let ask = Ask { action: "grep".into(), resources: vec![pattern.clone()], save: Some(vec!["*".into()]), metadata: Some(metadata) };
        context.assert(ask).await?;
        let root = target.absolute.clone();
        let kind = file_search::path_type(&root, &display).map_err(failure)?;
        // Anything else (a FIFO, a socket) would otherwise search the directory around it (D122).
        if kind != "file" && kind != "directory" {
            return Err(failure(format!("Search path is not a file or directory: {display}")));
        }
        let cwd = if kind == "directory" { root.clone() } else { dirname(&root) };
        let file = (kind == "file").then(|| crate::text::basename(&root).to_owned());
        let search = GrepInput {
            cwd: &cwd,
            pattern: &pattern,
            file: file.as_deref(),
            include: include.as_deref(),
            literal: literal == Some(true),
            case_sensitive,
        };
        let limit = limit.map(|l| l as usize).unwrap_or(DEFAULT_SEARCH_LIMIT);
        let (mut matches, truncated) = match file_search::bounded(limit, |limit| file_search::grep(&search, limit)).await {
            Ok(found) => found,
            Err(SearchError::Tool(message)) => return Err(failure(message)),
            Err(SearchError::InvalidPattern(message)) => return Err(failure(format!("Invalid regex pattern: {message}"))),
            Err(SearchError::Ripgrep(message)) => return Err(Failure::Error(ToolError { kind: "unknown", message, metadata: None })),
        };
        let directory = &context.location.directory;
        for item in &mut matches {
            let relative = crate::text::relative(directory, &crate::fs::resolve(&cwd, &item.path));
            item.path = crate::fs::resolve(directory, &relative);
        }
        let mut text = Obj::new();
        text.str("type", "text").str("text", &model_content(&matches, truncated));
        let mut metadata = Obj::new();
        metadata.int("matches", matches.len() as i64).set("truncated", Json::Bool(truncated));
        Ok(Output { content: vec![text.done()], metadata: Some(metadata.done()) })
    }
}

impl Tool for Grep {
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
    fn one_line_per_match() {
        let found = |line, text: &str| Match { path: "/project/a.txt".into(), line, text: text.into() };
        let matches = [found(1, "one\n"), found(2, "two\r\n"), found(3, "last")];
        assert_eq!(model_content(&matches, false), "Found 3 matches\n/project/a.txt:\n  Line 1: one\n  Line 2: two\n  Line 3: last");
    }
}
