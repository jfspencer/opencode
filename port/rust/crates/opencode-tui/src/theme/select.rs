//! `packages/theme/src/tui/select.ts`: the modes a document provides and the definition of one of them.

use super::value::{Rec, Value, merge_theme};

/// A color mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Light,
    Dark,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "light" => Some(Mode::Light),
            "dark" => Some(Mode::Dark),
            _ => None,
        }
    }
}

/// `themeModes(document)`: `light` and `dark`, those the document defines.
pub fn theme_modes(document: &Rec) -> Vec<Mode> {
    [Mode::Light, Mode::Dark].into_iter().filter(|mode| document.defined(mode.as_str()).is_some()).collect()
}

/// `supportsThemeMode(document, mode)`.
pub fn supports_theme_mode(document: &Rec, mode: Mode) -> bool {
    theme_modes(document).contains(&mode)
}

/// `selectThemeMode(document, mode = "light")`: the base merged with the mode's definition, and the mode used.
pub fn select_theme_mode(document: &Rec, mode: Option<Mode>) -> Result<(Rec, Mode), String> {
    let mode = mode.unwrap_or(Mode::Light);
    let modes = theme_modes(document);
    let selected_mode = if modes.contains(&mode) { Some(mode) } else { modes.first().copied() };
    let Some(selected_mode) = selected_mode else { return Err("Theme must provide at least one mode".into()) };
    let selected = document.get(selected_mode.as_str()).unwrap();
    if !selected.truthy() {
        return Err("Theme must provide at least one mode".into());
    }
    let base = document.get("base").cloned().unwrap_or(Value::Undefined);
    Ok((merge_theme(&[&base, selected]), selected_mode))
}
