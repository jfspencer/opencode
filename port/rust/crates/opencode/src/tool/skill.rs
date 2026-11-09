//! The skill tool (packages/core/src/tool/plugin/skill.ts): loads an inventory skill's instructions and a sample of its
//! files into the conversation (`Skill.prepare`, shared with prompt attachments in [`crate::session::skill`]), after
//! asking for the `skill` permission on its ID. An unknown skill, or a failure while preparing it, is
//! `Unable to load skill <id>` (unwrapped to its cause when it has one, e.g. a denied permission).

use opencode_protocol::Json;

use super::arguments::{self, Issue};
use super::{Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::json::Obj;

const NAME: &str = "skill";

pub struct Skill {
    pub definition: Definition,
}

/// `Schema.decodeUnknown(Input)`: `{id: Skill.ID}`.
fn decode(value: &Json) -> Result<String, Vec<Issue>> {
    let fields = arguments::object(value, "object").map_err(|issue| vec![issue])?;
    match fields.get("id") {
        Some(Json::String(id)) => Ok(id.clone()),
        Some(_) => Err(vec![Issue::new("id", "Expected Skill.ID")]),
        None => Err(vec![Issue::new("id", "Missing key")]),
    }
}

impl Tool for Skill {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        Box::pin(async move {
            let id = decode(&input).map_err(|issues| Failure::Error(arguments::invalid(NAME, &issues, &input)))?;
            let unable = || Failure::Error(ToolError::new(format!("Unable to load skill {id}")));
            let inventory = context.location.inventory();
            let Some(skill) = inventory.skill(&id).cloned() else { return Err(unable()) };
            context
                .assert(crate::permission::Ask {
                    action: NAME.into(),
                    resources: vec![skill.id.0.clone()],
                    save: Some(vec![skill.id.0.clone()]),
                    metadata: None,
                })
                .await
                .map_err(|failure| match failure {
                    Failure::Error(error) if error.message.is_empty() => unable(),
                    other => other,
                })?;
            let directory = if skill.path.0.starts_with("skill://") {
                crate::session::skill::uri(&skill.id.0, None)
            } else {
                crate::inventory::config::dirname(&skill.path.0)
            };
            let name = skill.name.0.clone();
            // Sampling the skill's files reads its directory.
            let output = tokio::task::spawn_blocking(move || crate::session::skill::prepare(&skill)).await.map_err(|_| unable())?;
            let mut text = Obj::new();
            text.str("type", "text").str("text", &output);
            let mut metadata = Obj::new();
            metadata.str("name", &name).str("directory", &directory);
            Ok(Output { content: vec![text.done()], metadata: Some(metadata.done()) })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    #[test]
    fn decoding() {
        assert_eq!(decode(&json::parse(r#"{"id":"deploy"}"#).unwrap()).ok().as_deref(), Some("deploy"));
        let issues = decode(&json::parse(r#"{"id":5}"#).unwrap()).err().unwrap();
        assert_eq!(issues[0].message, "Expected Skill.ID");
    }
}
