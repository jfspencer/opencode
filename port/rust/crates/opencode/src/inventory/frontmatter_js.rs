//! gray-matter's `javascript` frontmatter engine (lib/engines.js).

use super::jsvalue::Graph;

/// `engines.javascript.parse(block)`.
pub fn evaluate(_block: &str) -> Result<Graph, String> {
    Err("the javascript frontmatter engine is not evaluated".to_owned())
}
