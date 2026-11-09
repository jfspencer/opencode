//! The default code-mode interpreter (packages/codemode/src/interpreter): an AST walker whose program code runs as
//! Rust futures. Every program activation (a function call, a tool call's argument crossing, a promise reaction) is
//! polled by the runner (`runner.rs`); an `await`, a `yield`, a blocking host call, or a time slice surfaces as a
//! `Signal` that the nearest boundary able to handle it consumes, exactly where the TS interpreter splits its
//! explicit activation stack.

pub mod callback;
pub mod coerce;
pub mod collections;
pub mod cursor;
pub mod data;
pub mod errors;
pub mod eval;
pub mod expr;
pub mod generators;
pub mod interp;
pub mod limits;
pub mod native;
pub mod object;
pub mod promises;
pub mod runner;
pub mod scope;
pub mod value;
pub mod walk;

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

/// A program-side future: single-threaded, `'static`.
pub type Fut<T> = Pin<Box<dyn Future<Output = T>>>;

/// What interpreter work produces: a value, or the failure it throws.
pub type R<T> = Result<T, errors::Fail>;

/// The interpreter of one execution.
pub type Ctx = Rc<interp::Interp>;

/// A 1-based line and column in the submitted source (acorn's 0-based column + 1), as diagnostics report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    pub line: u32,
    pub column: u32,
}

impl Loc {
    pub fn of(span: crate::parser::ast::Span) -> Loc {
        Loc { line: span.loc_start.line, column: span.loc_start.column + 1 }
    }
}

pub fn boxed<T, F: Future<Output = T> + 'static>(future: F) -> Fut<T> {
    Box::pin(future)
}
