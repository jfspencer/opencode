//! The `execute` tool (packages/core/src/codemode/tool.ts `create`): runs a Code Mode program over the snapshot's
//! Code Mode inventory (see [`crate::codemode`]). Its definition is recorded from the reference
//! (`definitions/execute.json`); a snapshot builds one per request, holding the inventory it advertised.

use std::sync::Arc;

use opencode_protocol::Json;

use super::{Context, Definition, Execution, Failure, Tool, ToolError};
use crate::codemode::{self, Inventory};

pub struct Execute {
    definition: Definition,
    inventory: Arc<Inventory>,
}

impl Execute {
    pub fn new(definition: Definition, inventory: Arc<Inventory>) -> Arc<dyn Tool> {
        Arc::new(Execute { definition, inventory })
    }
}

impl Tool for Execute {
    fn definition(&self) -> &Definition {
        &self.definition
    }

    fn execute<'a>(&'a self, context: Context, input: Json) -> Execution<'a> {
        let code = match code(&input) {
            Ok(code) => code,
            Err(error) => return Box::pin(async move { Err(Failure::Error(error)) }),
        };
        Box::pin(codemode::pinned(codemode::execute(self.inventory.clone(), code, context)))
    }
}

/// The input decoded as `CodeMode.Input` (`{ code: string }`).
fn code(input: &Json) -> Result<String, ToolError> {
    let issue = match input {
        Json::Object(fields) => match fields.get("code") {
            Some(Json::String(code)) => return Ok(code.clone()),
            Some(_) => "- code: Expected string",
            None => "- code: Missing key",
        },
        _ => "- root: Expected object",
    };
    Err(ToolError::new(format!(
        "Invalid arguments for tool \"execute\":\n{issue}\n\nArguments provided:\n{}\n\nUpdate the arguments and call the tool again.",
        codemode::pretty(Some(input))
    )))
}
