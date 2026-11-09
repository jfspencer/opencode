//! A port of acorn 8.15.0 (the parser packages/codemode uses), with the options codemode passes:
//! `ecmaVersion: "latest"`, `sourceType: "script"`, `allowReturnOutsideFunction`, `allowAwaitOutsideFunction`,
//! `allowImportExportEverywhere`, `locations`.

pub mod ast;

/// Parses `source` (UTF-16 code units, as acorn reads a JS string) the way codemode calls acorn.
pub fn parse(source: &[u16]) -> Result<ast::Program, ast::SyntaxError> {
    let _ = source;
    Err(ast::SyntaxError { message: "parser not ported yet (1:0)".into(), pos: 0, loc: ast::Position { line: 1, column: 0 } })
}
