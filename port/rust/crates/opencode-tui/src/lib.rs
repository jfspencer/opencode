//! The opencode TUI (`packages/tui/src`), ported onto `opencode-tui-core`.
//!
//! - [`theme`]: `@opencode/theme` (`packages/theme/src/tui`) and `packages/tui/src/theme` (built-in themes, their
//!   resolution to colors, system and custom themes).
//! - [`config`]: `packages/tui/src/config` (`cli.json`, its defaults and keybinds).
pub mod config;
pub mod theme;
