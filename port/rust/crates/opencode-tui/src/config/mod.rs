//! The TUI's config: `packages/tui/src/config` (`Config.Info`, `Config.resolve`, `TuiKeybind`,
//! `newSessionLocation`) and the CLI config file the `opencode` command hands the TUI (`Config.Service` of
//! packages/cli/src/config).
//!
//! - [`file::CliConfig`]: `get()` and `update(mutation)` of `cli.json` (with `OPENCODE_CLI_CONFIG_CONTENT`), the
//!   `config` input of `run`.
//! - [`resolve::resolve`]: `Config.resolve`, the defaults (`mouse`, `leader.timeout`, `cursor`, `session`, `tabs`,
//!   `attention`) and the keybind bindings ([`keybind::BindingLookup`]).
//! - [`schema`]: Effect decoding of `Info`; [`jsonc`]: jsonc-parser 3.3.1 (parse, modify, format).
//!
//! The Solid side of index.tsx (`ConfigProvider` with its store and file watcher, `useConfig`, `useCursorStyle`) is
//! the app shell's: it keeps a [`resolve::Resolved`], re-resolves it from what `update` returns (`apply`), and
//! re-reads the file when its directory changes (`safeWatch`, `host.get()` then `apply`). Legacy v1 config
//! (`config/v1`, its migrations) is dropped.
pub mod file;
pub mod jsonc;
pub mod keybind;
pub mod resolve;
pub mod schema;

pub use file::CliConfig;
pub use keybind::{Binding, BindingLookup};
pub use resolve::{ResolveOptions, Resolved, resolve};

/// `LocationRef` of `@opencode/client`: a directory, and the workspace it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationRef {
    pub directory: String,
    pub workspace_id: Option<String>,
}

/// `newSessionLocation(mode, launchDirectory, current, unavailable)` (new-session-location.ts): where a new session
/// starts; `inherit` keeps the current location unless it is the unavailable one.
pub fn new_session_location(
    mode: &str,
    launch_directory: &str,
    current: Option<&LocationRef>,
    unavailable: Option<&LocationRef>,
) -> LocationRef {
    if mode == "inherit"
        && let Some(current) = current
        && (Some(&current.directory) != unavailable.map(|u| &u.directory)
            || current.workspace_id.as_ref() != unavailable.and_then(|u| u.workspace_id.as_ref()))
    {
        return current.clone();
    }
    LocationRef { directory: launch_directory.to_owned(), workspace_id: None }
}
