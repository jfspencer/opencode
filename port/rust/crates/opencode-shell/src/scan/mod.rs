//! `packages/core/src/shell/scan.ts`: splits a command line into the simple commands it runs, or declares it opaque.
//!
//! A transliteration of the TS scanner: the same functions in the same order, the same budget, the same failure
//! reasons. `bash.rs` is the POSIX family (Bash, Zsh, POSIX sh), `powershell.rs` PowerShell. Every position is a
//! UTF-16 offset, as in TS.

mod bash;
mod powershell;

use crate::js::JsString;

pub use bash::scan;
pub use powershell::scan_powershell;

/// Why the scanner cannot analyze a command (a parsing limitation, not a permission decision).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    CommandSubstitution,
    CompoundCommand,
    DynamicCommandName,
    DynamicExecution,
    Heredoc,
    InvalidRedirect,
    InvalidStructure,
    UnterminatedEscape,
    UnterminatedQuote,
}

impl Reason {
    /// The TS `OpaqueReason` literal.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::CommandSubstitution => "command-substitution",
            Reason::CompoundCommand => "compound-command",
            Reason::DynamicCommandName => "dynamic-command-name",
            Reason::DynamicExecution => "dynamic-execution",
            Reason::Heredoc => "heredoc",
            Reason::InvalidRedirect => "invalid-redirect",
            Reason::InvalidStructure => "invalid-structure",
            Reason::UnterminatedEscape => "unterminated-escape",
            Reason::UnterminatedQuote => "unterminated-quote",
        }
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One simple command the scanner found (TS `Command`). Optional TS fields are `None` / `false` when absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    /// The command's source text: what permission rules match.
    pub resource: JsString,
    /// Each word's cooked text (quotes removed, escapes decoded).
    pub words: Vec<JsString>,
    /// Each word's source text.
    pub raw_words: Vec<JsString>,
    /// PowerShell: exclusive raw-token ends relative to `resource`.
    pub word_ends: Option<Vec<isize>>,
    /// PowerShell: the command starts a statement.
    pub statement_head: bool,
    /// A declaration builtin (`export`, `declare`, ...), skipped as a permission resource.
    pub declaration: bool,
    /// Words before the first redirect, when redirects appear between arguments.
    pub redirect_word_count: Option<isize>,
    /// Exclusive prefix end relative to `resource`, for commandless or group redirects.
    pub redirect_prefix_end: Option<isize>,
}

/// TS `ShellScan.Result`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scan {
    Scanned(Vec<Command>),
    Opaque(Reason),
}

/// The POSIX shell family the command is read as. Posix covers sh, dash and any other shell, and stays sound for
/// Bash, Zsh and Dash alike, so it is the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Dialect {
    Bash,
    Zsh,
    #[default]
    Posix,
}

const MAX_INPUT_LENGTH: isize = 64 * 1024;
const MAX_SUBSTITUTION_DEPTH: isize = 32;
