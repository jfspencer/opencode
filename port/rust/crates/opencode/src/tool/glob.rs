//! The glob tool (packages/core/src/tool/plugin/glob.ts): file paths matching a glob under a directory of the
//! location (by default the location itself), listed as absolute paths.

use opencode_protocol::Json;

use super::file_access::{self, Kind};
use super::file_input::Wording;
use super::file_search::{self, DEFAULT_SEARCH_LIMIT, SearchError};
use super::{Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::json::{self, Obj};
use crate::permission::Ask;

pub struct Glob {
    pub definition: Definition,
}

/// `toModelContent`.
fn model_content(paths: &[String], truncated: bool) -> String {
    let mut lines: Vec<String> = if paths.is_empty() { vec!["No files found".into()] } else { paths.to_vec() };
    if truncated {
        lines.push(String::new());
        lines.push(format!(
            "(Results are truncated: showing first {} results. Consider using a more specific path or pattern.)",
            paths.len()
        ));
    }
    lines.join("\n")
}

impl Glob {
    async fn run(&self, context: Context, input: Json) -> Result<Output, Failure> {
        let input = super::file_input::decode(&self.definition, &input, &[("path", Wording::Expected("RelativePath"))])?;
        let pattern = json::get_str(&input, "pattern").unwrap_or_default().to_owned();
        let search_path = json::get_str(&input, "path").filter(|p| *p != "undefined" && *p != "null").map(str::to_owned);
        let hidden = json::get_bool(&input, "hidden");
        let limit = json::get_num(&input, "limit");
        let display = search_path.clone().unwrap_or_else(|| ".".into());
        let failure = |message: String| Failure::Error(ToolError::new(message));
        let target = file_access::resolve(&context.location, &display, Some(Kind::Directory));
        file_access::authorize_external(&context, "glob", &[&target], None).await?;
        let metadata = file_access::metadata(vec![
            ("root", Some(json::string(&display))),
            ("path", search_path.as_deref().map(json::string)),
            ("hidden", hidden.map(Json::Bool)),
            ("limit", limit.map(Json::Number)),
        ]);
        let ask = Ask { action: "glob".into(), resources: vec![pattern.clone()], save: Some(vec!["*".into()]), metadata: Some(metadata) };
        file_access::assert(&context, "glob", ask).await?;
        let kind = file_search::path_type(&target.absolute, &display).map_err(failure)?;
        if kind != "directory" {
            return Err(failure(format!("Search path is not a directory: {display}")));
        }
        let root = target.absolute.clone();
        let limit = limit.map(|l| l as usize).unwrap_or(DEFAULT_SEARCH_LIMIT);
        let searched =
            file_search::bounded(limit, |limit| file_search::glob(&root, &pattern, hidden == Some(true), limit)).await;
        let (paths, truncated) = match searched {
            Ok(found) => found,
            Err(SearchError::Tool(message)) => return Err(failure(message)),
            Err(SearchError::Ripgrep(message) | SearchError::InvalidPattern(message)) => {
                return Err(Failure::Error(ToolError { kind: "unknown", message, metadata: None }));
            }
        };
        // Relative to the location in the output, absolute for the model.
        let absolute: Vec<String> = paths
            .iter()
            .map(|path| crate::fs::resolve(&context.location.directory, &crate::text::relative(&context.location.directory, &crate::fs::resolve(&root, path))))
            .collect();
        let mut text = Obj::new();
        text.str("type", "text").str("text", &model_content(&absolute, truncated));
        let mut metadata = Obj::new();
        metadata.int("count", paths.len() as i64).set("truncated", Json::Bool(truncated));
        Ok(Output { content: vec![text.done()], metadata: Some(metadata.done()) })
    }
}

impl Tool for Glob {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(self.run(context, input))
    }
}
