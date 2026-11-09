//! The question tool (packages/core/src/tool/plugin/question.ts): asks the user one or more multiple-choice
//! questions through a form (`Questions`, one field `q<i>` per question, custom answers allowed) and tells the model
//! the answers.
//!
//! The call needs the `question` permission. A dismissed form declines the call (the turn stops); a dismissal with
//! a message is a tool error carrying it, so the model continues. An interrupted call cancels its form.

use opencode_protocol::Json;

use super::{Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::form::{CreateInput, State};
use crate::json::{self, Obj};
use crate::permission::Ask;

pub struct Question {
    pub definition: Definition,
}

/// `QuestionTool.CancelledError`'s message.
const DISMISSED: &str = "The user dismissed this question";

/// One decode issue of the input (`StandardSchemaV1.Issue`): its path and message.
struct Issue {
    path: Vec<PathSegment>,
    message: &'static str,
}

#[derive(Clone, Copy)]
enum PathSegment {
    Key(&'static str),
    Index(usize),
}

fn issue(path: &[PathSegment], last: Option<PathSegment>, message: &'static str) -> Issue {
    let mut path = path.to_vec();
    path.extend(last);
    Issue { path, message }
}

/// A struct's string field (`Schema.String`).
fn string_field(object: &opencode_protocol::wire::Object, key: &'static str, path: &[PathSegment], issues: &mut Vec<Issue>) {
    match object.get(key) {
        None => issues.push(issue(path, Some(PathSegment::Key(key)), "Missing key")),
        Some(Json::String(_)) => {}
        Some(_) => issues.push(issue(path, Some(PathSegment::Key(key)), "Expected string")),
    }
}

/// `Schema.decodeUnknownEffect(QuestionTool.Input)(input, { errors: "all" })`'s issues, in Effect's order (struct
/// keys in declaration order, array items in order).
fn input_issues(input: &Json) -> Vec<Issue> {
    let mut issues = Vec::new();
    let Some(root) = input.as_object() else {
        return vec![issue(&[], None, "Expected object")];
    };
    let questions = match root.get("questions") {
        // The non-empty check reports a missing key as well.
        None => return vec![issue(&[], Some(PathSegment::Key("questions")), "Expected a value with a length of at least 1")],
        Some(Json::Array(items)) => items,
        Some(_) => return vec![issue(&[], Some(PathSegment::Key("questions")), "Expected array")],
    };
    for (index, question) in questions.iter().enumerate() {
        let path = [PathSegment::Key("questions"), PathSegment::Index(index)];
        let Some(prompt) = question.as_object() else {
            issues.push(issue(&path, None, "Expected Question.Prompt"));
            continue;
        };
        string_field(prompt, "question", &path, &mut issues);
        string_field(prompt, "header", &path, &mut issues);
        match prompt.get("options") {
            None => issues.push(issue(&path, Some(PathSegment::Key("options")), "Missing key")),
            Some(Json::Array(options)) => {
                for (option_index, option) in options.iter().enumerate() {
                    let option_path =
                        [PathSegment::Key("questions"), PathSegment::Index(index), PathSegment::Key("options"), PathSegment::Index(option_index)];
                    let Some(option) = option.as_object() else {
                        issues.push(issue(&option_path, None, "Expected object"));
                        continue;
                    };
                    string_field(option, "label", &option_path, &mut issues);
                    string_field(option, "description", &option_path, &mut issues);
                }
            }
            Some(_) => issues.push(issue(&path, Some(PathSegment::Key("options")), "Expected array")),
        }
        if prompt.get("multiple").is_some_and(|multiple| !matches!(multiple, Json::Bool(_))) {
            issues.push(issue(&path, Some(PathSegment::Key("multiple")), "Expected boolean"));
        }
    }
    if issues.is_empty() && questions.is_empty() {
        issues.push(issue(&[], Some(PathSegment::Key("questions")), "Expected a value with a length of at least 1"));
    }
    issues
}

/// `JSON.stringify(value, null, 2)`.
fn pretty(value: &Json, indent: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                pretty(item, indent + 1, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        Json::Object(fields) if !fields.is_empty() => {
            out.push_str("{\n");
            for (i, (key, item)) in fields.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                out.push_str(&json::text(&json::string(key)));
                out.push_str(": ");
                pretty(item, indent + 1, out);
                out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        other => out.push_str(&json::text(other)),
    }
}

/// `formatInputIssues` (tool/runtime.ts): the model-facing message of an invalid input.
fn format_issues(tool: &str, issues: &[Issue], value: &Json) -> String {
    let mut details: Vec<String> = issues
        .iter()
        .take(5)
        .map(|issue| {
            let mut path = String::new();
            for segment in &issue.path {
                match segment {
                    PathSegment::Index(index) => path.push_str(&format!("[{index}]")),
                    PathSegment::Key(key) if path.is_empty() => path.push_str(key),
                    PathSegment::Key(key) => path.push_str(&format!(".{key}")),
                }
            }
            if path.is_empty() {
                path.push_str("root");
            }
            format!("- {path}: {}", issue.message)
        })
        .collect();
    if issues.len() > 5 {
        let more = issues.len() - 5;
        details.push(format!("- ...and {more} more {}", if more == 1 { "issue" } else { "issues" }));
    }
    let mut provided = String::new();
    pretty(value, 0, &mut provided);
    format!(
        "Invalid arguments for tool \"{tool}\":\n{}\n\nArguments provided:\n{provided}\n\nUpdate the arguments and call the tool again.",
        details.join("\n")
    )
}

/// `toField`: question `index` as a form field.
fn to_field(question: &Json, index: usize) -> Json {
    let options = json::get(question, "options").and_then(json::array).unwrap_or_default();
    let mut o = Obj::new();
    o.str("key", &format!("q{index}"));
    o.opt("title", json::get(question, "header").cloned());
    o.opt("description", json::get(question, "question").cloned());
    o.str("type", if json::get_bool(question, "multiple") == Some(true) { "multiselect" } else { "string" });
    o.set(
        "options",
        Json::Array(
            options
                .iter()
                .map(|option| {
                    let mut item = Obj::new();
                    item.opt("value", json::get(option, "label").cloned());
                    item.opt("label", json::get(option, "label").cloned());
                    item.opt("description", json::get(option, "description").cloned());
                    item.done()
                })
                .collect(),
        ),
    );
    o.set("custom", Json::Bool(true));
    o.done()
}

/// `String(value)` of an answer value.
fn js_string(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Number(n) => opencode_protocol::wire::number::js_number_string(*n),
        Json::Bool(b) => b.to_string(),
        Json::Null => "null".into(),
        Json::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        Json::Object(_) => "[object Object]".into(),
    }
}

/// `toModelContent`.
fn model_content(questions: &[Json], answers: &[Vec<String>]) -> String {
    let formatted: Vec<String> = questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let answer = match answers.get(index) {
                Some(answer) if !answer.is_empty() => answer.join(", "),
                _ => "Unanswered".to_owned(),
            };
            format!("\"{}\"=\"{answer}\"", json::get_str(question, "question").unwrap_or_default())
        })
        .collect();
    format!("User has answered your questions: {}. You can now continue with the user's answers in mind.", formatted.join(", "))
}

impl Tool for Question {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(async move {
            let issues = input_issues(&input);
            if !issues.is_empty() {
                return Err(Failure::Error(ToolError::new(format_issues(&self.definition.name, &issues, &input))));
            }
            let questions = json::get(&input, "questions").and_then(json::array).unwrap_or_default();
            // `ToolFailure("Permission denied: question", error)`: a denial or a rejection with feedback shows through
            // the wrapper, a plain rejection declines the call, and a missing session (no message) shows the wrapper.
            context.assert(Ask { action: "question".into(), resources: vec!["*".into()], save: None, metadata: None }).await.map_err(
                |failure| match failure {
                    Failure::Error(error) if error.kind == "tool.execution" => {
                        Failure::Error(ToolError::new("Permission denied: question"))
                    }
                    other => other,
                },
            )?;
            let mut tool = Obj::new();
            tool.str("messageID", &context.message_id).str("id", &context.call_id);
            let mut metadata = Obj::new();
            metadata.str("kind", "question").set("tool", tool.done());
            let form = CreateInput {
                id: None,
                session_id: context.session_id.clone(),
                title: "Questions".into(),
                metadata: Some(metadata.done()),
                fields: Json::Array(questions.iter().enumerate().map(|(index, question)| to_field(question, index)).collect()),
            };
            let location = &context.location;
            let state = location.forms.ask(&context.app.bus, &location.reference(), form).await.map_err(|error| {
                // `Effect.orDie`: the fields are the tool's own.
                Failure::Error(ToolError::new(error.message()))
            })?;
            let answer = match state {
                State::Cancelled(Some(message)) => return Err(Failure::Error(ToolError::new(message))),
                State::Cancelled(None) | State::Pending => {
                    return Err(Failure::Declined { question: true, message: DISMISSED.into() });
                }
                State::Answered(answer) => answer,
            };
            let answers: Vec<Vec<String>> = (0..questions.len())
                .map(|index| match json::get(&answer, &format!("q{index}")) {
                    None => vec![],
                    Some(Json::Array(items)) => items.iter().map(js_string).collect(),
                    Some(value) => vec![js_string(value)],
                })
                .collect();
            let answers_json = || {
                Json::Array(answers.iter().map(|answer| Json::Array(answer.iter().map(|a| json::string(a)).collect())).collect())
            };
            let mut text = Obj::new();
            text.str("type", "text").str("text", &model_content(questions, &answers));
            let mut metadata = Obj::new();
            metadata.set("answers", answers_json());
            Ok(Output { content: vec![text.done()], metadata: Some(metadata.done()) })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(input: &str) -> Vec<String> {
        let input = json::parse(input).unwrap();
        let issues = input_issues(&input);
        format_issues("question", &issues, &input).lines().filter(|l| l.starts_with("- ")).map(str::to_owned).collect()
    }

    #[test]
    fn issues() {
        assert_eq!(messages(r#"{"questions":[]}"#), ["- questions: Expected a value with a length of at least 1"]);
        assert_eq!(messages(r#"{}"#), ["- questions: Expected a value with a length of at least 1"]);
        assert_eq!(messages(r#"[]"#), ["- root: Expected object"]);
        assert_eq!(
            messages(r#"{"questions":[{"question":1,"header":"h","options":[{"label":2}],"multiple":"y"}]}"#),
            [
                "- questions[0].question: Expected string",
                "- questions[0].options[0].label: Expected string",
                "- questions[0].options[0].description: Missing key",
                "- questions[0].multiple: Expected boolean"
            ]
        );
        assert_eq!(messages(r#"{"questions":[1,2,3,4,5,6]}"#).last().unwrap(), "- ...and 1 more issue");
        assert_eq!(messages(r#"{"questions":[1,2,3,4,5,6,7]}"#).last().unwrap(), "- ...and 2 more issues");
        let input = json::parse(r#"{"questions":[]}"#).unwrap();
        assert_eq!(
            format_issues("question", &input_issues(&input), &input),
            "Invalid arguments for tool \"question\":\n- questions: Expected a value with a length of at least 1\n\nArguments provided:\n{\n  \"questions\": []\n}\n\nUpdate the arguments and call the tool again."
        );
    }
}
