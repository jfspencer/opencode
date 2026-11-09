//! Output helpers: `JSON.stringify(value, null, 2)`, the reports of a failed command (`Cause.pretty` without
//! stack frames, which the standard drops anyway) and the line writers the TS commands use.

use std::io::Write;

use opencode_protocol::Json;

/// `JSON.stringify(value)` of a string.
pub fn quote(text: &str) -> String {
    crate::json::text(&Json::String(text.to_owned()))
}

/// `JSON.stringify(value, null, 2)`.
pub fn pretty(value: &Json) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &Json, depth: usize, out: &mut String) {
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&"  ".repeat(depth + 1));
                write_pretty(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
            out.push(']');
        }
        Json::Object(object) if !object.is_empty() => {
            out.push('{');
            for (index, (key, item)) in object.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&"  ".repeat(depth + 1));
                out.push_str(&quote(key));
                out.push_str(": ");
                write_pretty(item, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
            out.push('}');
        }
        other => out.push_str(&crate::json::text(other)),
    }
}

/// A command's failure: what `Cause.pretty` prints for it (`name: message`), and exit code 1.
#[derive(Debug)]
pub struct Fail {
    pub name: String,
    pub message: String,
}

impl Fail {
    pub fn error(message: impl Into<String>) -> Self {
        Self { name: "Error".into(), message: message.into() }
    }
    pub fn named(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self { name: name.into(), message: message.into() }
    }
}

impl From<super::client::ClientError> for Fail {
    fn from(error: super::client::ClientError) -> Self {
        Fail::named(error.name(), error.message())
    }
}

pub type Result<T = i32> = std::result::Result<T, Fail>;

/// `process.stdout.write`.
pub fn stdout(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// `process.stderr.write`.
pub fn stderr(text: &str) {
    let mut out = std::io::stderr().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}
