//! The opencode TUI (`packages/tui/src`), ported onto `opencode-tui-core`.
//!
//! - [`app`]: `run` and `<App>`; [`ctx`]: the providers' values; [`home`], [`prompt`], [`local`], [`keymap`],
//!   [`session`], [`data`]: the home route and what it reads; [`util`]: locale and path helpers.
//! - [`theme`]: `@opencode/theme` (`packages/theme/src/tui`) and `packages/tui/src/theme` (built-in themes, their
//!   resolution to colors, system and custom themes).
//! - [`config`]: `packages/tui/src/config` (`cli.json`, its defaults and keybinds).
pub mod app;
pub mod config;
pub mod ctx;
pub mod data;
pub mod home;
pub mod keymap;
pub mod local;
pub mod prompt;
pub mod session;
pub mod theme;
pub mod util;
