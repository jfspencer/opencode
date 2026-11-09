//! Code mode: the confined JavaScript-like interpreter behind opencode's `execute` tool, ported from
//! packages/codemode (the default AST interpreter, `src/interpreter`, and its standard library). See README.md.

pub mod jsstr;
pub mod parser;
