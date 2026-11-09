//! `packages/tui/src/ui`: the dialog framework ([`dialog`], [`dialog_select`]) and what dialogs share
//! ([`theme`]: `useTheme` with the dialog surface; [`locale`]: truncation; [`scroll`]: the wheel acceleration).
pub mod dialog;
pub mod dialog_help;
pub mod dialog_prompt;
pub mod dialog_select;
pub mod link;
pub mod locale;
pub mod scroll;
pub mod theme;
