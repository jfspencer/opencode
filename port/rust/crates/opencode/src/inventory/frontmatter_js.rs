//! gray-matter's `javascript` frontmatter engine (`---js`, `---javascript`; lib/engines.js), which the port does not
//! evaluate: such a file is skipped, as one whose engine throws.
//!
//! In TS the engine is a direct `eval` of `(function() {\nreturn <frontmatter>;\n}());` inside gray-matter's
//! strict-mode `parse(str, options, wrap)`, its result `|| {}`; on an error whose message matches
//! `/(unexpected|identifier)/i` it evaluates the same wrapped text once more. So the frontmatter is code with the
//! server's privileges and no time limit: it sees gray-matter's locals (`---js\nparse` is gray-matter's own `parse`
//! function, `str` the wrapped source, `options` its options), the module's `require`, `module`, `exports`,
//! `__dirname`, and every Bun global (`process`, `Bun`, `Buffer`, `globalThis`, `fetch`), and what it changes there
//! persists in the server process from one file to the next.
//!
//! Out of scope: none of that has a counterpart in the port, and the Code Mode interpreter (`opencode-codemode`) is
//! not JavaScriptCore: a confined subset (no classes, getters or setters, user constructors, BigInt or symbols; its
//! own `require`, `tools`, `search` and `console`; no `globalThis` or `process`) checked against the TS Code Mode
//! interpreter, whose results cross a JSON boundary (functions and `undefined` fields vanish, a `Date` becomes its
//! string), where the consumers see the live value (jsvalue.rs).
//!
//! TODO(port): a frontmatter that is a pure expression over the standard library (object and array literals,
//! arithmetic, string and array methods) evaluates to the same value in the interpreter. Evaluating that subset
//! needs a scope analysis telling it apart (free references to the host's names above) and a bridge handing the
//! interpreter's live value graph to [`super::jsvalue`] before the run frees it.

use super::jsvalue::Graph;

/// `engines.javascript.parse(block)`: not evaluated (see the module docs).
pub fn evaluate(_block: &str) -> Result<Graph, String> {
    Err("the javascript frontmatter engine is not evaluated".to_owned())
}
