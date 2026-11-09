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

/// Parses `source` (UTF-16 code units, as acorn reads a JS string) the way codemode calls acorn.
pub fn parse(source: &[u16]) -> Result<ast::Program, ast::SyntaxError> {
    let mut parser = state::Parser::new(source);
    parser.parse_program().map_err(|fail| match fail {
        state::Fail::Syntax(error) => *error,
        // Only raised while reading a template chunk, where the tokenizer catches it.
        state::Fail::TemplateEscape => ast::SyntaxError {
            message: "Bad escape sequence in untagged template literal (1:0)".into(),
            pos: 0,
            loc: ast::Position { line: 1, column: 0 },
        },
    })
}
