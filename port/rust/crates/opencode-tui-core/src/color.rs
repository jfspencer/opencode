//! Colors and text attributes: `lib/RGBA.ts`, the color half of `ansi.zig`, `TextAttributes` (`types.ts`,
//! `ansi.zig`) and the link-id packing of `utils.ts`.
//!
//! An [`RGBA`] is OpenTUI 0.8's packed color: four `u16`, each an 8-bit channel in the low byte and one byte of
//! a 32-bit metadata word in the high byte. The metadata holds the color's intent (literal RGB, an indexed
//! palette slot, or the terminal's default color) and the slot, so the renderer emits `38;2`, `38;5` or `39`
//! as the color was made, and equality is exact integer equality, as in TS and Zig.
//!
//! The TS getters `r`, `g`, `b`, `a` return the channel as a float (`byte / 255`); the setters quantize a
//! float with `Math.round`. Both are here as `r()`/`set_r()` and so on.
//!
//! Divergences: `hexToRgb` warns on the console about an invalid hex color before returning magenta; there is
//! no console here, so only the magenta is returned.

/// The packed color (`RGBA` in `lib/RGBA.ts` and `ansi.zig`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RGBA(pub [u16; 4]);

/// How a color reaches the terminal (`ColorIntent`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorIntent {
    /// A literal color: `38;2;r;g;b` (or its nearest palette entry where the terminal has no true color).
    Rgb,
    /// A palette slot: `38;5;n` where the terminal has 256 colors; the channels are its RGB snapshot.
    Indexed,
    /// The terminal's default foreground or background: `39`/`49`.
    Default,
}

const INTENT_RGB: u32 = 0;
const INTENT_INDEXED: u32 = 1;
const INTENT_DEFAULT: u32 = 2;

/// The default foreground's RGB (`DEFAULT_FOREGROUND_RGB`).
pub const DEFAULT_FOREGROUND_RGB: [u8; 3] = [255, 255, 255];
/// The default background's RGB (`DEFAULT_BACKGROUND_RGB`).
pub const DEFAULT_BACKGROUND_RGB: [u8; 3] = [0, 0, 0];

/// The standard 16-color palette (`ANSI16_RGB`).
pub const ANSI16_RGB: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0x80, 0x00, 0x00],
    [0x00, 0x80, 0x00],
    [0x80, 0x80, 0x00],
    [0x00, 0x00, 0x80],
    [0x80, 0x00, 0x80],
    [0x00, 0x80, 0x80],
    [0xc0, 0xc0, 0xc0],
    [0x80, 0x80, 0x80],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x00, 0x00, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

/// The levels of the 6x6x6 color cube (`ANSI_256_CUBE_LEVELS`).
pub const ANSI_256_CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// `packMeta`: bits 0-7 the slot, bits 8-15 the intent.
pub const fn pack_meta(intent: ColorIntent, slot: u8) -> u32 {
    let intent = match intent {
        ColorIntent::Rgb => INTENT_RGB,
        ColorIntent::Indexed => INTENT_INDEXED,
        ColorIntent::Default => INTENT_DEFAULT,
    };
    (slot as u32) | (intent << 8)
}

/// JS `Math.round` of a value already clamped to be non-negative.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// `toU8`: a 0..1 float channel to a byte.
fn to_u8(value: f64) -> u8 {
    let value = if value.is_finite() { value } else { 0.0 };
    js_round(value.clamp(0.0, 1.0) * 255.0) as u8
}

/// `toByte`: a 0..255 number to a byte.
fn to_byte(value: f64) -> u8 {
    let value = if value.is_finite() { value } else { 0.0 };
    js_round(value.clamp(0.0, 255.0)) as u8
}

impl RGBA {
    /// `packRGBA8`.
    pub const fn pack(r: u8, g: u8, b: u8, a: u8, meta: u32) -> Self {
        RGBA([
            r as u16 | (((meta & 0xff) as u16) << 8),
            g as u16 | ((((meta >> 8) & 0xff) as u16) << 8),
            b as u16 | ((((meta >> 16) & 0xff) as u16) << 8),
            a as u16 | ((((meta >> 24) & 0xff) as u16) << 8),
        ])
    }

    /// `ansi.rgbColor`: a literal color from bytes.
    pub const fn rgb(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::pack(r, g, b, a, pack_meta(ColorIntent::Rgb, 0))
    }

    /// `RGBA.fromValues`: from 0..1 floats (alpha defaults to 1 in TS).
    pub fn from_values(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self::rgb(to_u8(r), to_u8(g), to_u8(b), to_u8(a))
    }

    /// `RGBA.fromInts`: from 0..255 numbers (alpha defaults to 255 in TS).
    pub fn from_ints(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self::rgb(to_byte(r), to_byte(g), to_byte(b), to_byte(a))
    }

    /// `RGBA.fromHex`.
    pub fn from_hex(hex: &str) -> Self {
        hex_to_rgb(hex)
    }

    /// `RGBA.fromIndex`: a palette slot, its channels from `snapshot` or the standard palette.
    pub fn from_index(index: u8, snapshot: Option<RGBA>) -> Self {
        let [r, g, b, a] = snapshot.unwrap_or_else(|| {
            let [r, g, b] = ansi256_index_to_rgb(index);
            RGBA::rgb(r, g, b, 255)
        }).to_ints();
        Self::pack(r, g, b, a, pack_meta(ColorIntent::Indexed, index))
    }

    /// `RGBA.defaultForeground`.
    pub fn default_foreground(snapshot: Option<RGBA>) -> Self {
        let [r, g, b] = DEFAULT_FOREGROUND_RGB;
        let [r, g, b, a] = snapshot.unwrap_or(RGBA::rgb(r, g, b, 255)).to_ints();
        Self::pack(r, g, b, a, pack_meta(ColorIntent::Default, 0))
    }

    /// `RGBA.defaultBackground`.
    pub fn default_background(snapshot: Option<RGBA>) -> Self {
        let [r, g, b] = DEFAULT_BACKGROUND_RGB;
        let [r, g, b, a] = snapshot.unwrap_or(RGBA::rgb(r, g, b, 255)).to_ints();
        Self::pack(r, g, b, a, pack_meta(ColorIntent::Default, 0))
    }

    /// `toInts`: the four channel bytes.
    pub const fn to_ints(&self) -> [u8; 4] {
        [self.0[0] as u8, self.0[1] as u8, self.0[2] as u8, self.0[3] as u8]
    }

    pub const fn red(&self) -> u8 {
        self.0[0] as u8
    }
    pub const fn green(&self) -> u8 {
        self.0[1] as u8
    }
    pub const fn blue(&self) -> u8 {
        self.0[2] as u8
    }
    pub const fn alpha(&self) -> u8 {
        self.0[3] as u8
    }

    /// The `r` getter: the channel as a 0..1 float.
    pub fn r(&self) -> f64 {
        self.red() as f64 / 255.0
    }
    pub fn g(&self) -> f64 {
        self.green() as f64 / 255.0
    }
    pub fn b(&self) -> f64 {
        self.blue() as f64 / 255.0
    }
    pub fn a(&self) -> f64 {
        self.alpha() as f64 / 255.0
    }

    /// The `r` setter: keeps the metadata byte.
    pub fn set_r(&mut self, value: f64) {
        self.0[0] = (self.0[0] & 0xff00) | to_u8(value) as u16;
    }
    pub fn set_g(&mut self, value: f64) {
        self.0[1] = (self.0[1] & 0xff00) | to_u8(value) as u16;
    }
    pub fn set_b(&mut self, value: f64) {
        self.0[2] = (self.0[2] & 0xff00) | to_u8(value) as u16;
    }
    pub fn set_a(&mut self, value: f64) {
        self.0[3] = (self.0[3] & 0xff00) | to_u8(value) as u16;
    }

    /// `meta` / `ansi.getMeta`.
    pub const fn meta(&self) -> u32 {
        (self.0[0] >> 8) as u32 | (((self.0[1] >> 8) as u32) << 8) | (((self.0[2] >> 8) as u32) << 16) | (((self.0[3] >> 8) as u32) << 24)
    }

    /// `intent`.
    pub const fn intent(&self) -> ColorIntent {
        match (self.meta() >> 8) & 0xff {
            INTENT_INDEXED => ColorIntent::Indexed,
            INTENT_DEFAULT => ColorIntent::Default,
            _ => ColorIntent::Rgb,
        }
    }

    /// `slot`: the palette slot of an indexed color.
    pub const fn slot(&self) -> u8 {
        self.meta() as u8
    }

    /// `ansi.withMeta`: the same channels with other metadata.
    pub const fn with_meta(&self, meta: u32) -> Self {
        Self::pack(self.red(), self.green(), self.blue(), self.alpha(), meta)
    }

    /// `equals`.
    pub fn equals(&self, other: Option<&RGBA>) -> bool {
        other.is_some_and(|other| self == other)
    }

    /// `ansi.rgbaToRgb24`: 0xRRGGBB.
    pub const fn to_rgb24(&self) -> u32 {
        ((self.red() as u32) << 16) | ((self.green() as u32) << 8) | self.blue() as u32
    }

    /// `validateColor` (buffer.zig): the metadata is exactly what a constructor makes.
    pub fn is_valid(&self) -> bool {
        let intent = self.intent();
        self.meta() == pack_meta(intent, if intent == ColorIntent::Indexed { self.slot() } else { 0 })
    }
}

impl std::fmt::Debug for RGBA {
    /// `toString`, with the intent where it is not literal RGB.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rgba({:.2}, {:.2}, {:.2}, {:.2})", self.r(), self.g(), self.b(), self.a())?;
        match self.intent() {
            ColorIntent::Rgb => Ok(()),
            ColorIntent::Indexed => write!(f, "[{}]", self.slot()),
            ColorIntent::Default => write!(f, "[default]"),
        }
    }
}

/// `normalizeIndexedColorIndex` is a range check in TS; a `u8` is always in range.
pub fn ansi256_index_to_rgb(index: u8) -> [u8; 3] {
    let index = index as usize;
    if index < ANSI16_RGB.len() {
        return ANSI16_RGB[index];
    }
    if index < 232 {
        let cube = index - 16;
        return [ANSI_256_CUBE_LEVELS[cube / 36], ANSI_256_CUBE_LEVELS[(cube / 6) % 6], ANSI_256_CUBE_LEVELS[cube % 6]];
    }
    let value = (8 + (index - 232) * 10) as u8;
    [value, value, value]
}

/// `ansi.fallbackAnsi256Color`: the standard palette entry as a literal color.
pub fn fallback_ansi256_color(index: u8) -> RGBA {
    let [r, g, b] = ansi256_index_to_rgb(index);
    RGBA::rgb(r, g, b, 255)
}

/// `ansi.colorDistanceSquared` (alpha ignored).
pub fn color_distance_squared(a: RGBA, b: RGBA) -> f32 {
    let dr = a.red() as f32 - b.red() as f32;
    let dg = a.green() as f32 - b.green() as f32;
    let db = a.blue() as f32 - b.blue() as f32;
    dr * dr + dg * dg + db * db
}

/// `hexToRgb`: `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` (the `#` optional); magenta otherwise.
pub fn hex_to_rgb(hex: &str) -> RGBA {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    let chars: Vec<char> = hex.chars().collect();
    let expanded: String = match chars.len() {
        3 | 4 => chars.iter().flat_map(|c| [*c, *c]).collect(),
        _ => hex.to_owned(),
    };
    let valid = (expanded.len() == 6 || expanded.len() == 8) && expanded.bytes().all(|b| b.is_ascii_hexdigit());
    if !valid {
        return RGBA::from_values(1.0, 0.0, 1.0, 1.0);
    }
    let byte = |i: usize| u8::from_str_radix(&expanded[i..i + 2], 16).unwrap_or(0);
    let a = if expanded.len() == 8 { byte(6) } else { 255 };
    RGBA::rgb(byte(0), byte(2), byte(4), a)
}

/// `rgbToHex`: `#rrggbb`, or `#rrggbbaa` when not opaque.
pub fn rgb_to_hex(rgb: RGBA) -> String {
    let [r, g, b, a] = rgb.to_ints();
    if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
}

/// `hsvToRgb` (RGBA.ts): hue in degrees, saturation and value 0..1.
pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> RGBA {
    let i = ((h / 60.0).floor() as i64).rem_euclid(6);
    let f = h / 60.0 - (h / 60.0).floor();
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    RGBA::from_values(r, g, b, 1.0)
}

const CSS_COLOR_NAMES: &[(&str, &str)] = &[
    ("black", "#000000"),
    ("white", "#FFFFFF"),
    ("red", "#FF0000"),
    ("green", "#008000"),
    ("blue", "#0000FF"),
    ("yellow", "#FFFF00"),
    ("cyan", "#00FFFF"),
    ("magenta", "#FF00FF"),
    ("silver", "#C0C0C0"),
    ("gray", "#808080"),
    ("grey", "#808080"),
    ("maroon", "#800000"),
    ("olive", "#808000"),
    ("lime", "#00FF00"),
    ("aqua", "#00FFFF"),
    ("teal", "#008080"),
    ("navy", "#000080"),
    ("fuchsia", "#FF00FF"),
    ("purple", "#800080"),
    ("orange", "#FFA500"),
    ("brightblack", "#666666"),
    ("brightred", "#FF6666"),
    ("brightgreen", "#66FF66"),
    ("brightblue", "#6666FF"),
    ("brightyellow", "#FFFF66"),
    ("brightcyan", "#66FFFF"),
    ("brightmagenta", "#FF66FF"),
    ("brightwhite", "#FFFFFF"),
];

/// A `ColorInput`: a color string or an [`RGBA`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorInput<'a> {
    Str(&'a str),
    Rgba(RGBA),
}

impl<'a> From<&'a str> for ColorInput<'a> {
    fn from(value: &'a str) -> Self {
        ColorInput::Str(value)
    }
}

impl From<RGBA> for ColorInput<'_> {
    fn from(value: RGBA) -> Self {
        ColorInput::Rgba(value)
    }
}

/// `parseColor`: `transparent`, a CSS color name (case-insensitive) or a hex color; an [`RGBA`] as is.
pub fn parse_color<'a>(color: impl Into<ColorInput<'a>>) -> RGBA {
    match color.into() {
        ColorInput::Rgba(rgba) => rgba,
        ColorInput::Str(text) => {
            // JS toLowerCase: the names are ASCII, so only an ASCII-lowercased match can hit.
            let lower = text.to_lowercase();
            if lower == "transparent" {
                return RGBA::from_values(0.0, 0.0, 0.0, 0.0);
            }
            if let Some((_, hex)) = CSS_COLOR_NAMES.iter().find(|(name, _)| *name == lower) {
                return hex_to_rgb(hex);
            }
            hex_to_rgb(text)
        }
    }
}

/// `TextAttributes` (`types.ts`): the base attribute bits of a cell.
pub mod text_attributes {
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

/// `ATTRIBUTE_BASE_BITS`.
pub const ATTRIBUTE_BASE_BITS: u32 = 8;
/// `ATTRIBUTE_BASE_MASK`.
pub const ATTRIBUTE_BASE_MASK: u32 = 0xff;
const LINK_ID_SHIFT: u32 = 8;
const LINK_ID_PAYLOAD_MASK: u32 = 0xff_ffff;

/// The flags of `createTextAttributes` (`utils.ts`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextAttributeFlags {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub blink: bool,
    pub inverse: bool,
    pub reverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

/// `createTextAttributes`.
pub fn create_text_attributes(flags: TextAttributeFlags) -> u32 {
    use text_attributes::*;
    let mut attributes = NONE;
    if flags.bold {
        attributes |= BOLD;
    }
    if flags.italic {
        attributes |= ITALIC;
    }
    if flags.underline {
        attributes |= UNDERLINE;
    }
    if flags.dim {
        attributes |= DIM;
    }
    if flags.blink {
        attributes |= BLINK;
    }
    if flags.inverse || flags.reverse {
        attributes |= INVERSE;
    }
    if flags.hidden {
        attributes |= HIDDEN;
    }
    if flags.strikethrough {
        attributes |= STRIKETHROUGH;
    }
    attributes
}

/// `getBaseAttributes`: the low 8 bits.
pub const fn base_attributes(attributes: u32) -> u32 {
    attributes & ATTRIBUTE_BASE_MASK
}

/// `attributesWithLink` / `TextAttributes.setLinkId`: base bits plus a link id in bits 8-31.
pub const fn attributes_with_link(base: u32, link_id: u32) -> u32 {
    (base & ATTRIBUTE_BASE_MASK) | ((link_id & LINK_ID_PAYLOAD_MASK) << LINK_ID_SHIFT)
}

/// `getLinkId`.
pub const fn get_link_id(attributes: u32) -> u32 {
    (attributes >> LINK_ID_SHIFT) & LINK_ID_PAYLOAD_MASK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing() {
        let c = RGBA::from_index(15, None);
        assert_eq!(c.to_ints(), [255, 255, 255, 255]);
        assert_eq!(c.intent(), ColorIntent::Indexed);
        assert_eq!(c.slot(), 15);
        assert!(c.is_valid());
        assert_eq!(RGBA::from_values(0.5, 0.0, 1.0, 1.0).to_ints(), [128, 0, 255, 255]);
        assert_eq!(parse_color("#abc").to_ints(), [0xaa, 0xbb, 0xcc, 255]);
        assert_eq!(parse_color("transparent").alpha(), 0);
        assert_eq!(parse_color("Orange").to_ints(), [255, 165, 0, 255]);
        assert_eq!(parse_color("nope").to_ints(), [255, 0, 255, 255]);
        assert_eq!(ansi256_index_to_rgb(232), [8, 8, 8]);
        assert_eq!(ansi256_index_to_rgb(16 + 36 + 6 + 1), [95, 95, 95]);
    }
}
