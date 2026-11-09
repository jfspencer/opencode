//! The style data the ported code reads and writes: `RGBA` (lib/RGBA.ts), `StyleDefinition`/`MergedStyle` and the
//! definitions side of `SyntaxStyle` (syntax-style.ts), `TextChunk` (text-buffer.ts), `StyledText`
//! (lib/styled-text.ts) and `createTextAttributes` (utils.ts).
//!
//! Divergences: `SyntaxStyle` here keeps only the definitions (`styleDefs`) that `getStyle` and `mergeStyles`
//! read; registering a style with the native core (style ids) is the renderer's. Chunk text is a [`JsString`].

use crate::js::JsString;
use std::collections::HashMap;

/// `RGBA`: four 16-bit words, each an 8-bit channel with 8 bits of colour metadata (intent, palette slot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rgba(pub [u16; 4]);

fn to_byte(v: f64) -> u16 {
    (if v.is_finite() { v } else { 0.0 }).clamp(0.0, 255.0).round() as u16
}

impl Rgba {
    /// `RGBA.fromInts(r, g, b, a)` (intent rgb).
    pub fn from_ints(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        Rgba([r as u16, g as u16, b as u16, a as u16])
    }

    /// `RGBA.fromValues(r, g, b, a)` with channels in 0..1.
    pub fn from_values(r: f64, g: f64, b: f64, a: f64) -> Rgba {
        let u8v = |v: f64| to_byte((if v.is_finite() { v } else { 0.0 }).clamp(0.0, 1.0) * 255.0);
        Rgba([u8v(r), u8v(g), u8v(b), u8v(a)])
    }

    /// `hexToRgb(hex)`; an invalid string is magenta, as in TS.
    pub fn from_hex(hex: &str) -> Rgba {
        let h = hex.strip_prefix('#').unwrap_or(hex);
        let chars: Vec<char> = h.chars().collect();
        let h: String = match chars.len() {
            3 => [chars[0], chars[0], chars[1], chars[1], chars[2], chars[2]].iter().collect(),
            4 => [chars[0], chars[0], chars[1], chars[1], chars[2], chars[2], chars[3], chars[3]].iter().collect(),
            _ => h.to_owned(),
        };
        let valid = (h.len() == 6 || h.len() == 8) && h.chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            return Rgba::from_values(1.0, 0.0, 1.0, 1.0);
        }
        let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap();
        Rgba::from_ints(p(0), p(2), p(4), if h.len() == 8 { p(6) } else { 255 })
    }
}

/// `TextAttributes`.
pub mod attributes {
    pub const NONE: u32 = 0;
    pub const BOLD: u32 = 1 << 0;
    pub const DIM: u32 = 1 << 1;
    pub const ITALIC: u32 = 1 << 2;
    pub const UNDERLINE: u32 = 1 << 3;
    pub const BLINK: u32 = 1 << 4;
    pub const INVERSE: u32 = 1 << 5;
    pub const HIDDEN: u32 = 1 << 6;
    pub const STRIKETHROUGH: u32 = 1 << 7;
}

/// `StyleDefinition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct StyleDefinition {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub dim: Option<bool>,
}

impl StyleDefinition {
    /// `createTextAttributes({ bold, italic, underline, dim })` of this style.
    pub fn attributes(&self) -> u32 {
        create_text_attributes(self.bold, self.italic, self.underline, self.dim)
    }
}

/// `createTextAttributes` for the four flags styles carry.
pub fn create_text_attributes(bold: Option<bool>, italic: Option<bool>, underline: Option<bool>, dim: Option<bool>) -> u32 {
    let mut a = attributes::NONE;
    if bold == Some(true) {
        a |= attributes::BOLD;
    }
    if italic == Some(true) {
        a |= attributes::ITALIC;
    }
    if underline == Some(true) {
        a |= attributes::UNDERLINE;
    }
    if dim == Some(true) {
        a |= attributes::DIM;
    }
    a
}

/// `MergedStyle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MergedStyle {
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
}

/// The definitions of a `SyntaxStyle`, keyed by scope name in registration order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyntaxStyle {
    names: Vec<String>,
    defs: HashMap<String, StyleDefinition>,
}

impl SyntaxStyle {
    pub fn new() -> SyntaxStyle {
        SyntaxStyle::default()
    }

    /// `SyntaxStyle.fromStyles(styles)`.
    pub fn from_styles<'a>(styles: impl IntoIterator<Item = (&'a str, StyleDefinition)>) -> SyntaxStyle {
        let mut s = SyntaxStyle::new();
        for (name, def) in styles {
            s.register_style(name, def);
        }
        s
    }

    /// `registerStyle(name, style)` (the definitions part).
    pub fn register_style(&mut self, name: &str, style: StyleDefinition) {
        if !self.defs.contains_key(name) {
            self.names.push(name.to_owned());
        }
        self.defs.insert(name.to_owned(), style);
    }

    /// `getStyle(name)`: the style, else the style of the base scope (before the first `.`).
    pub fn get_style(&self, name: &str) -> Option<StyleDefinition> {
        if let Some(s) = self.defs.get(name) {
            return Some(*s);
        }
        if name.contains('.') {
            let base = name.split('.').next().unwrap();
            return self.defs.get(base).copied();
        }
        None
    }

    /// `mergeStyles(...names)`.
    pub fn merge_styles(&self, names: &[&str]) -> MergedStyle {
        let mut d = StyleDefinition::default();
        for name in names {
            let Some(style) = self.get_style(name) else { continue };
            if style.fg.is_some() {
                d.fg = style.fg;
            }
            if style.bg.is_some() {
                d.bg = style.bg;
            }
            if style.bold.is_some() {
                d.bold = style.bold;
            }
            if style.italic.is_some() {
                d.italic = style.italic;
            }
            if style.underline.is_some() {
                d.underline = style.underline;
            }
            if style.dim.is_some() {
                d.dim = style.dim;
            }
        }
        MergedStyle { fg: d.fg, bg: d.bg, attributes: d.attributes() }
    }

    /// `getRegisteredNames()`.
    pub fn registered_names(&self) -> &[String] {
        &self.names
    }
}

/// `TextChunk`. `link` is `{ url }`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TextChunk {
    pub text: JsString,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    /// `attributes` (absent in a chunk from `stringToStyledText`).
    pub attributes: Option<u32>,
    pub link: Option<JsString>,
}

impl TextChunk {
    /// A chunk styled with `style` (or unstyled with attributes 0), as the ported code builds them.
    pub fn styled(text: JsString, style: Option<&StyleDefinition>) -> TextChunk {
        TextChunk {
            text,
            fg: style.and_then(|s| s.fg),
            bg: style.and_then(|s| s.bg),
            attributes: Some(style.map_or(0, |s| s.attributes())),
            link: None,
        }
    }
}

/// `StyledText`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct StyledText {
    pub chunks: Vec<TextChunk>,
}

impl StyledText {
    pub fn new(chunks: Vec<TextChunk>) -> StyledText {
        StyledText { chunks }
    }

    /// `stringToStyledText(content)`: one chunk without style or attributes.
    pub fn from_string(content: JsString) -> StyledText {
        StyledText { chunks: vec![TextChunk { text: content, ..TextChunk::default() }] }
    }

    /// The text of all chunks.
    pub fn plain_text(&self) -> String {
        self.chunks.iter().map(|c| c.text.to_string()).collect()
    }
}
