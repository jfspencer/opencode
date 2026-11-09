//! `ui/border.ts`: `EmptyBorder` and `SplitBorder` (a left bar `┃`).
use opencode_tui_core::border::{BorderCharacters, border_chars_to_array};

/// `EmptyBorder`.
pub const EMPTY_BORDER: BorderCharacters = BorderCharacters {
    top_left: "",
    top_right: "",
    bottom_left: "",
    bottom_right: "",
    horizontal: " ",
    vertical: "",
    top_t: "",
    bottom_t: "",
    left_t: "",
    right_t: "",
    cross: "",
};

/// `SplitBorder.customBorderChars`.
pub fn split_border_chars() -> [u32; 11] {
    border_chars_to_array(&BorderCharacters { vertical: "┃", ..EMPTY_BORDER })
}
