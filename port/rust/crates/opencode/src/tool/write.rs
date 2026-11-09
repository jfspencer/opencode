//! The write tool (packages/core/src/tool/plugin/write.ts): a whole file written (its parent directories created),
//! approved as an `edit` of the file with the change as the preview, then formatted.

use opencode_protocol::Json;

use super::file_write::{self, WriteFailure};
use super::{Context, Definition, Execution, Failure, Output, Tool, ToolError};
use crate::json::{self, Obj};

pub struct Write {
    pub definition: Definition,
}

impl Write {
    async fn run(&self, context: Context, input: Json) -> Result<Output, Failure> {
        let input = super::file_input::decode(&self.definition, &input, &[])?;
        let path = json::get_str(&input, "path").unwrap_or_default();
        let content = json::get_str(&input, "content").unwrap_or_default();
        let (target, written) = match file_write::write(&context, path, content).await {
            Ok(done) => done,
            Err(WriteFailure::Tool(failure)) => return Err(failure),
            Err(WriteFailure::Read) => return Err(Failure::Error(ToolError::new(format!("Unable to write {path}")))),
        };
        let verb = if written.existed { "Wrote" } else { "Created" };
        let mut text = Obj::new();
        text.str("type", "text").str("text", &format!("{verb} file successfully: {}", target.resource));
        Ok(Output { content: vec![text.done()], metadata: None })
    }
}

impl Tool for Write {
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
