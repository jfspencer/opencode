//! The todowrite tool (packages/core/src/tool/plugin/todo.ts): the model's working plan, replaced whole on every
//! call and shown back as a summary; registered only while the location's `todo` flag is on (`experimental.todo`).

use std::sync::{Arc, OnceLock};

use opencode_protocol::Json;

use super::{Context, Definition, Execution, Failure, Output, Tool};
use crate::json::{self, Obj};
use crate::permission::Ask;

pub const NAME: &str = "todowrite";

pub struct Todo {
    definition: Definition,
}

/// The tools the plugin registers in a location: todowrite while the `todo` flag is on.
pub fn tools(inventory: &crate::inventory::Inventory) -> Vec<Arc<dyn Tool>> {
    static TOOL: OnceLock<Arc<dyn Tool>> = OnceLock::new();
    if !crate::flag::enabled(&inventory.entries, "todo") {
        return vec![];
    }
    let tool = TOOL.get_or_init(|| Arc::new(Todo { definition: super::definition(NAME, include_str!("definitions/todowrite.json")) }));
    vec![tool.clone()]
}

/// `summarize`: the counts by status, and the items in progress.
fn summarize(todos: &[Json]) -> String {
    if todos.is_empty() {
        return "Todos cleared".into();
    }
    let count = |status: &str| todos.iter().filter(|todo| json::get_str(todo, "status") == Some(status)).count();
    let mut counts = vec![format!("{}/{} completed", count("completed"), todos.len())];
    for status in ["in_progress", "pending", "cancelled"] {
        let n = count(status);
        if n > 0 {
            counts.push(format!("{n} {}", status.replacen('_', " ", 1)));
        }
    }
    let active: Vec<String> = todos
        .iter()
        .filter(|todo| json::get_str(todo, "status") == Some("in_progress"))
        .map(|todo| json::text(&json::string(json::get_str(todo, "content").unwrap_or_default())))
        .collect();
    let in_progress = if active.is_empty() { String::new() } else { format!(". In progress: {}", active.join(", ")) };
    format!("Todos updated: {}{in_progress}", counts.join(", "))
}

impl Todo {
    async fn run(&self, context: Context, input: Json) -> Result<Output, Failure> {
        let input = super::file_input::decode(&self.definition, &input, &[])?;
        let ask = Ask { action: NAME.into(), resources: vec!["*".into()], save: Some(vec!["*".into()]), metadata: None };
        // `ToolFailure({message: "Unable to update todos", error})`: the session error is the permission error's own
        // (`toSessionError` unwraps the cause), and a declined call still stops the turn.
        context.assert(ask).await?;
        let todos = json::get(&input, "todos").and_then(json::array).map(<[Json]>::to_vec).unwrap_or_default();
        let mut text = Obj::new();
        text.str("type", "text").str("text", &summarize(&todos));
        let mut metadata = Obj::new();
        metadata.set("todos", Json::Array(todos));
        Ok(Output { content: vec![text.done()], metadata: Some(metadata.done()) })
    }
}

impl Tool for Todo {
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
    fn summaries() {
        let todo = |content: &str, status: &str| {
            let mut o = Obj::new();
            o.str("content", content).str("status", status);
            o.done()
        };
        assert_eq!(summarize(&[]), "Todos cleared");
        assert_eq!(
            summarize(&[todo("a", "completed"), todo("b \"x\"", "in_progress"), todo("c", "pending")]),
            "Todos updated: 1/3 completed, 1 in progress, 1 pending. In progress: \"b \\\"x\\\"\""
        );
    }
}
