//! `RGBA` from `@opentui/core` 0.8.1 (`packages/core/src/lib/RGBA.ts`), as far as the theme code uses it.
//!
//! TODO(port-tui): opencode_tui_core::color::RGBA. This is a stand-in until the core's color module lands; it keeps
//! RGBA.ts's representation (8-bit channels plus a meta byte per channel: intent and palette slot), so swapping it is
//! a type replacement.
//!
//! Channels are stored as bytes: `from_values` rounds `value * 255`, and `r()`..`a()` read `byte / 255`, exactly as
//! RGBA.ts does since 0.8 (its buffer is a `Uint16Array` whose low bytes are the channels).

use super::js;

/// A color: four 8-bit channels and their meta bytes (`meta` packs intent and slot, RGBA.ts `packMeta`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RGBA {
    /// `buffer[i] & 0xff` of RGBA.ts.
    pub channels: [u8; 4],
    /// `buffer[i] >>> 8` of RGBA.ts.
    pub meta: [u8; 4],
}

impl std::fmt::Debug for RGBA {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

const INTENT_RGB: u32 = 0;

fn pack_meta(intent: u32, slot: u32) -> u32 {
    (slot & 0xff) | ((intent & 0xff) << 8)
}

/// `toU8`: a 0..1 value to a byte.
fn to_u8(value: f64) -> f64 {
    let value = if value.is_finite() { value } else { 0.0 };
    js::round(value.clamp(0.0, 1.0) * 255.0)
}

/// `toByte`: a 0..255 value to a byte (non-finite is 0).
fn to_byte(value: f64) -> u8 {
    let value = if value.is_finite() { value } else { 0.0 };
    (js::round(value.clamp(0.0, 255.0)) as i64 & 0xff) as u8
}

fn pack(r: f64, g: f64, b: f64, a: f64, meta: u32) -> RGBA {
    RGBA {
        channels: [to_byte(r), to_byte(g), to_byte(b), to_byte(a)],
        meta: [(meta & 0xff) as u8, ((meta >> 8) & 0xff) as u8, ((meta >> 16) & 0xff) as u8, ((meta >> 24) & 0xff) as u8],
    }
}

impl RGBA {
    /// `RGBA.fromValues(r, g, b, a = 1)`: channels in 0..1.
    pub fn from_values(r: f64, g: f64, b: f64, a: f64) -> RGBA {
        pack(to_u8(r), to_u8(g), to_u8(b), to_u8(a), pack_meta(INTENT_RGB, 0))
    }

    /// `RGBA.fromInts(r, g, b, a = 255)`.
    pub fn from_ints(r: f64, g: f64, b: f64, a: f64) -> RGBA {
        pack(r, g, b, a, pack_meta(INTENT_RGB, 0))
    }

    /// `RGBA.fromHex` (`hexToRgb`): `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`; anything else is magenta (with a
    /// console warning in TS).
    pub fn from_hex(hex: &str) -> RGBA {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        let chars: Vec<char> = hex.chars().collect();
        let expanded: String = match chars.len() {
            3 => [chars[0], chars[0], chars[1], chars[1], chars[2], chars[2]].iter().collect(),
            4 => [chars[0], chars[0], chars[1], chars[1], chars[2], chars[2], chars[3], chars[3]].iter().collect(),
            _ => hex.to_owned(),
        };
        let valid = (expanded.len() == 6 || expanded.len() == 8) && expanded.bytes().all(|b| b.is_ascii_hexdigit());
        if !valid {
            return RGBA::from_values(1.0, 0.0, 1.0, 1.0);
        }
        let byte = |i: usize| u8::from_str_radix(&expanded[i..i + 2], 16).unwrap() as f64;
        let a = if expanded.len() == 8 { byte(6) } else { 255.0 };
        RGBA::from_ints(byte(0), byte(2), byte(4), a)
    }

    /// `toInts()`.
    pub fn to_ints(&self) -> [u8; 4] {
        self.channels
    }

    pub fn r(&self) -> f64 {
        self.channels[0] as f64 / 255.0
    }

    pub fn g(&self) -> f64 {
        self.channels[1] as f64 / 255.0
    }

    pub fn b(&self) -> f64 {
        self.channels[2] as f64 / 255.0
    }

    pub fn a(&self) -> f64 {
        self.channels[3] as f64 / 255.0
    }

    /// `equals`: channels and meta.
    pub fn equals(&self, other: &RGBA) -> bool {
        self == other
    }

    /// `rgbToHex`: `#rrggbb`, with `aa` unless opaque.
    pub fn to_hex(&self) -> String {
        let [r, g, b, a] = self.channels;
        if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
    }
}

#[cfg(test)]
mod tests {
    use super::RGBA;

    #[test]
    fn hex() {
        assert_eq!(RGBA::from_hex("#abc").to_hex(), "#aabbcc");
        assert_eq!(RGBA::from_hex("#abcd").to_hex(), "#aabbccdd");
        assert_eq!(RGBA::from_hex("#12345678").to_ints(), [0x12, 0x34, 0x56, 0x78]);
        assert_eq!(RGBA::from_hex("nope").to_hex(), "#ff00ff");
        assert_eq!(RGBA::from_values(0.5, 0.0, 1.0, 1.0).to_ints(), [128, 0, 255, 255]);
    }
}
