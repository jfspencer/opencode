//! A port of acorn 8.15.0 (the parser packages/codemode uses), with the options codemode passes:
//! `ecmaVersion: "latest"`, `sourceType: "script"`, `allowReturnOutsideFunction`, `allowAwaitOutsideFunction`,
//! `allowImportExportEverywhere`, `locations`.
//!
//! The port follows acorn function by function (tokenizer with its token contexts, statement and expression
//! parsers, `toAssignable` / `checkLVal*`, scopes, the regular expression validator) so that acceptance, every
//! error message and position, and the tree match acorn's. `harness/parser/check.sh` compares the two on a corpus.

pub mod ast;
pub mod estree;

mod chars;
mod expression;
mod ident_tables;
mod lval;
mod regexp;
mod state;
mod statement;
mod tokenizer;

#[cfg(test)]
mod tests;

/// The message of the error [`parse`] returns where acorn would exhaust the JS stack (a `RangeError` there, not a
/// `SyntaxError`); it has no ` (line:column)` suffix. acorn under bun manages about 3,900 nested parentheses or
/// 35,000 nested blocks; the default limit allows a few hundred to a few thousand levels, depending on the construct.
pub const STACK_OVERFLOW_MESSAGE: &str = "Maximum call stack size exceeded";

/// How much stack [`parse`] may use below its caller's frame: safe on a thread with a 2 MiB stack.
pub const DEFAULT_STACK_LIMIT: usize = 1024 * 1024;

/// Parses `source` (UTF-16 code units, as acorn reads a JS string) the way codemode calls acorn.
pub fn parse(source: &[u16]) -> Result<ast::Program, ast::SyntaxError> {
    parse_with_stack_limit(source, DEFAULT_STACK_LIMIT)
}

/// [`parse`], recursing at most `stack_limit` bytes below the caller's frame before failing with
/// [`STACK_OVERFLOW_MESSAGE`].
pub fn parse_with_stack_limit(source: &[u16], stack_limit: usize) -> Result<ast::Program, ast::SyntaxError> {
    let mut parser = state::Parser::new(source, stack_limit);
    parser.parse_program().map_err(|fail| match fail {
        state::Fail::Syntax(error) => *error,
        // Only raised while reading a template chunk, where the tokenizer catches it.
        state::Fail::TemplateEscape => unreachable!("invalid template escapes are caught by the template reader"),
    })
}
