//! Color math: OKLCH conversion (`packages/theme/src/tui/color.ts`), the ANSI palette and `tint`
//! (`packages/tui/src/theme/color.ts`).
//!
//! The float operations are the same IEEE operations in the same order as the TS, with the platform libm for
//! `cbrt`, `powf`, `atan2`, `sin` and `cos` (JavaScriptCore calls the same functions on Linux).

use super::js;
use super::rgba::RGBA;

/// A color in OKLCH (`l` 0..1, `c` chroma, `h` degrees).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    pub l: f64,
    pub c: f64,
    pub h: f64,
}

fn hue(value: f64) -> f64 {
    ((value % 360.0) + 360.0) % 360.0
}

fn linear_to_srgb(value: f64) -> f64 {
    if value <= 0.0031308 {
        return value * 12.92;
    }
    1.055 * value.powf(1.0 / 2.4) - 0.055
}

fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        return value / 12.92;
    }
    ((value + 0.055) / 1.055).powf(2.4)
}

/// `rgbToOklch(red, green, blue)`: channels in 0..1.
pub fn rgb_to_oklch(red: f64, green: f64, blue: f64) -> Oklch {
    let lr = srgb_to_linear(red);
    let lg = srgb_to_linear(green);
    let lb = srgb_to_linear(blue);
    let l_root = (0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb).cbrt();
    let m_root = (0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb).cbrt();
    let s_root = (0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb).cbrt();
    let lightness = 0.2104542553 * l_root + 0.793617785 * m_root - 0.0040720468 * s_root;
    let a = 1.9779984951 * l_root - 2.428592205 * m_root + 0.4505937099 * s_root;
    let b = 0.0259040371 * l_root + 0.7827717662 * m_root - 0.808675766 * s_root;
    let chroma = (a * a + b * b).sqrt();
    let angle = b.atan2(a) * (180.0 / std::f64::consts::PI);
    Oklch { l: lightness, c: chroma, h: if angle < 0.0 { angle + 360.0 } else { angle } }
}

struct Rgb {
    r: f64,
    g: f64,
    b: f64,
}

fn oklch_to_rgb(color: Oklch) -> Rgb {
    let a = color.c * ((color.h * std::f64::consts::PI) / 180.0).cos();
    let b = color.c * ((color.h * std::f64::consts::PI) / 180.0).sin();
    let l_root = color.l + 0.3963377774 * a + 0.2158037573 * b;
    let m_root = color.l - 0.1055613458 * a - 0.0638541728 * b;
    let s_root = color.l - 0.0894841775 * a - 1.291485548 * b;
    let l = l_root * l_root * l_root;
    let m = m_root * m_root * m_root;
    let s = s_root * s_root * s_root;
    Rgb {
        r: linear_to_srgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
        g: linear_to_srgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
        b: linear_to_srgb(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
    }
}

fn in_gamut(rgb: &Rgb) -> bool {
    (0.0..=1.0).contains(&rgb.r) && (0.0..=1.0).contains(&rgb.g) && (0.0..=1.0).contains(&rgb.b)
}

fn fit_oklch(color: Oklch) -> Oklch {
    let base = Oklch { l: js_clamp(color.l, 0.0, 1.0), c: js::max(0.0, color.c), h: hue(color.h) };
    if in_gamut(&oklch_to_rgb(base)) {
        return base;
    }
    for index in 0..24 {
        let next = Oklch { c: base.c * js_pow(0.9, (index + 1) as f64), ..base };
        if in_gamut(&oklch_to_rgb(next)) {
            return next;
        }
    }
    Oklch { c: 0.0, ..base }
}

/// `Math.pow`.
pub fn js_pow(base: f64, exponent: f64) -> f64 {
    base.powf(exponent)
}

/// `Math.max(min, Math.min(max, value))`, NaN-propagating like JS.
fn js_clamp(value: f64, min: f64, max: f64) -> f64 {
    js::max(min, js::min(max, value))
}

/// `oklchToHex`: `#rrggbb` of the color fitted into the sRGB gamut.
pub fn oklch_to_hex(color: Oklch) -> String {
    let rgb = oklch_to_rgb(fit_oklch(color));
    let byte = |value: f64| {
        let clamped = js_clamp(value, 0.0, 1.0);
        let n = js::round(clamped * 255.0);
        if n.is_nan() { "NaN".to_owned() } else { format!("{:02x}", n as i64) }
    };
    format!("#{}{}{}", byte(rgb.r), byte(rgb.g), byte(rgb.b))
}

/// `ansiToRgba(code)` (packages/tui/src/theme/color.ts).
pub fn ansi_to_rgba(code: f64) -> RGBA {
    const ANSI: [&str; 16] = [
        "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0", "#808080", "#ff0000",
        "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    if code < 16.0 {
        // `ansiColors[code] ?? "#000000"`: only an integer index finds an entry.
        let hex = if code >= 0.0 && code.fract() == 0.0 { ANSI[code as usize] } else { "#000000" };
        return RGBA::from_hex(hex);
    }
    if code < 232.0 {
        let index = code - 16.0;
        let b = index % 6.0;
        let g = (index / 6.0).floor() % 6.0;
        let r = (index / 36.0).floor();
        let val = |x: f64| if x == 0.0 { 0.0 } else { x * 40.0 + 55.0 };
        return RGBA::from_ints(val(r), val(g), val(b), 255.0);
    }
    if code < 256.0 {
        let gray = (code - 232.0) * 10.0 + 8.0;
        return RGBA::from_ints(gray, gray, gray, 255.0);
    }
    RGBA::from_ints(0.0, 0.0, 0.0, 255.0)
}

/// `tint(base, overlay, alpha)`: `base` moved toward `overlay` by `alpha`, opaque.
pub fn tint(base: RGBA, overlay: RGBA, alpha: f64) -> RGBA {
    let r = base.r() + (overlay.r() - base.r()) * alpha;
    let g = base.g() + (overlay.g() - base.g()) * alpha;
    let b = base.b() + (overlay.b() - base.b()) * alpha;
    RGBA::from_ints(js::round(r * 255.0), js::round(g * 255.0), js::round(b * 255.0), 255.0)
}
