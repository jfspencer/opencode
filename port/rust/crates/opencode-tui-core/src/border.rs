//! Box borders: `lib/border.ts` (styles, characters, sides) and the packed draw options of `buffer.ts`.
//!
//! Divergences: `parseBorderStyle` warns on the console about an invalid style before falling back; there is
//! no console here, so only the fallback is returned.

/// `BorderStyle`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum BorderStyle {
    #[default]
    Single,
    Double,
    Rounded,
    Heavy,
}

impl BorderStyle {
    /// The style's name, as TS spells it.
    pub fn name(self) -> &'static str {
        match self {
            BorderStyle::Single => "single",
            BorderStyle::Double => "double",
            BorderStyle::Rounded => "rounded",
            BorderStyle::Heavy => "heavy",
        }
    }

    /// The index of the style in native tables (`single`, `double`, `rounded`, `heavy`).
    pub fn index(self) -> u32 {
        match self {
            BorderStyle::Single => 0,
            BorderStyle::Double => 1,
            BorderStyle::Rounded => 2,
            BorderStyle::Heavy => 3,
        }
    }

    /// The style's characters (`BorderChars[style]`).
    pub fn chars(self) -> &'static BorderCharacters {
        match self {
            BorderStyle::Single => &SINGLE,
            BorderStyle::Double => &DOUBLE,
            BorderStyle::Rounded => &ROUNDED,
            BorderStyle::Heavy => &HEAVY,
        }
    }

    /// The style's characters as code points (`BorderCharArrays[style]`).
    pub fn char_array(self) -> [u32; 11] {
        border_chars_to_array(self.chars())
    }
}

/// `isValidBorderStyle` + the parse: a style name.
pub fn border_style_from_name(name: &str) -> Option<BorderStyle> {
    match name {
        "single" => Some(BorderStyle::Single),
        "double" => Some(BorderStyle::Double),
        "rounded" => Some(BorderStyle::Rounded),
        "heavy" => Some(BorderStyle::Heavy),
        _ => None,
    }
}

/// `parseBorderStyle`: the named style, or `fallback`.
pub fn parse_border_style(value: Option<&str>, fallback: BorderStyle) -> BorderStyle {
    value.and_then(border_style_from_name).unwrap_or(fallback)
}

/// `BorderCharacters`: one string per position (only the first code point is drawn).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorderCharacters {
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub horizontal: &'static str,
    pub vertical: &'static str,
    pub top_t: &'static str,
    pub bottom_t: &'static str,
    pub left_t: &'static str,
    pub right_t: &'static str,
    pub cross: &'static str,
}

const SINGLE: BorderCharacters = BorderCharacters {
    top_left: "┌",
    top_right: "┐",
    bottom_left: "└",
    bottom_right: "┘",
    horizontal: "─",
    vertical: "│",
    top_t: "┬",
    bottom_t: "┴",
    left_t: "├",
    right_t: "┤",
    cross: "┼",
};
const DOUBLE: BorderCharacters = BorderCharacters {
    top_left: "╔",
    top_right: "╗",
    bottom_left: "╚",
    bottom_right: "╝",
    horizontal: "═",
    vertical: "║",
    top_t: "╦",
    bottom_t: "╩",
    left_t: "╠",
    right_t: "╣",
    cross: "╬",
};
const ROUNDED: BorderCharacters = BorderCharacters {
    top_left: "╭",
    top_right: "╮",
    bottom_left: "╰",
    bottom_right: "╯",
    horizontal: "─",
    vertical: "│",
    top_t: "┬",
    bottom_t: "┴",
    left_t: "├",
    right_t: "┤",
    cross: "┼",
};
const HEAVY: BorderCharacters = BorderCharacters {
    top_left: "┏",
    top_right: "┓",
    bottom_left: "┗",
    bottom_right: "┛",
    horizontal: "━",
    vertical: "┃",
    top_t: "┳",
    bottom_t: "┻",
    left_t: "┣",
    right_t: "┫",
    cross: "╋",
};

/// The positions of a border character array (`BorderCharIndex` in buffer.zig).
pub mod border_char_index {
    pub const TOP_LEFT: usize = 0;
    pub const TOP_RIGHT: usize = 1;
    pub const BOTTOM_LEFT: usize = 2;
    pub const BOTTOM_RIGHT: usize = 3;
    pub const HORIZONTAL: usize = 4;
    pub const VERTICAL: usize = 5;
    pub const TOP_T: usize = 6;
    pub const BOTTOM_T: usize = 7;
    pub const LEFT_T: usize = 8;
    pub const RIGHT_T: usize = 9;
    pub const CROSS: usize = 10;
}

fn first_code_point(text: &str) -> u32 {
    // `codePointAt(0) ?? 0x20`: an empty string draws a blank cell.
    text.chars().next().map(|c| c as u32).unwrap_or(0x20)
}

/// `borderCharsToArray`.
pub fn border_chars_to_array(chars: &BorderCharacters) -> [u32; 11] {
    [
        first_code_point(chars.top_left),
        first_code_point(chars.top_right),
        first_code_point(chars.bottom_left),
        first_code_point(chars.bottom_right),
        first_code_point(chars.horizontal),
        first_code_point(chars.vertical),
        first_code_point(chars.top_t),
        first_code_point(chars.bottom_t),
        first_code_point(chars.left_t),
        first_code_point(chars.right_t),
        first_code_point(chars.cross),
    ]
}

/// `BorderSidesConfig`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BorderSides {
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    pub left: bool,
}

impl BorderSides {
    pub const ALL: BorderSides = BorderSides { top: true, right: true, bottom: true, left: true };
    pub const NONE: BorderSides = BorderSides { top: false, right: false, bottom: false, left: false };

    /// `getBorderSides`: `true` is every side; a list names sides.
    pub fn from_border(border: &Border) -> BorderSides {
        match border {
            Border::All(true) => BorderSides::ALL,
            Border::All(false) => BorderSides::NONE,
            Border::Sides(sides) => BorderSides {
                top: sides.contains(&Side::Top),
                right: sides.contains(&Side::Right),
                bottom: sides.contains(&Side::Bottom),
                left: sides.contains(&Side::Left),
            },
        }
    }

    pub fn any(&self) -> bool {
        self.top || self.right || self.bottom || self.left
    }

    /// The native bit set: left 1, bottom 2, right 4, top 8 (`OT_BORDER_*`, and `packDrawOptions`' low bits).
    pub fn bits(&self) -> u32 {
        (self.left as u32) | ((self.bottom as u32) << 1) | ((self.right as u32) << 2) | ((self.top as u32) << 3)
    }

    /// The sides of a native bit set.
    pub fn from_bits(bits: u32) -> BorderSides {
        BorderSides { left: bits & 1 != 0, bottom: bits & 2 != 0, right: bits & 4 != 0, top: bits & 8 != 0 }
    }
}

/// `BorderSides` (the string union): one side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

/// `boolean | BorderSides[]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Border {
    All(bool),
    Sides(Vec<Side>),
}

/// `getBorderFromSides`.
pub fn get_border_from_sides(sides: BorderSides) -> Border {
    let mut result = Vec::new();
    if sides.top {
        result.push(Side::Top);
    }
    if sides.right {
        result.push(Side::Right);
    }
    if sides.bottom {
        result.push(Side::Bottom);
    }
    if sides.left {
        result.push(Side::Left);
    }
    if result.is_empty() { Border::All(false) } else { Border::Sides(result) }
}

/// A title's alignment (`"left" | "center" | "right"`; 0, 1, 2 natively).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TitleAlignment {
    #[default]
    Left,
    Center,
    Right,
}

impl TitleAlignment {
    pub fn code(self) -> u8 {
        match self {
            TitleAlignment::Left => 0,
            TitleAlignment::Center => 1,
            TitleAlignment::Right => 2,
        }
    }
}
