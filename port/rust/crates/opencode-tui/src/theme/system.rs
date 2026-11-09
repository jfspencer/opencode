//! The "system" theme, generated from the terminal's palette (`packages/tui/src/theme/system.ts`).

use super::color::{ansi_to_rgba, tint};
use super::resolve::ThemeError;
use super::rgba::RGBA;
use super::value::{Color, Rec, Value};

/// `TerminalColors` of `@opentui/core`, as far as the theme reads it: the palette (index 0..15, `None` where the
/// terminal did not answer) and the default colors, as hex strings.
///
/// TODO(port-tui): opencode_tui_core's terminal palette type, once the core's palette detection lands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalColors {
    pub palette: Vec<Option<String>>,
    pub default_foreground: Option<String>,
    pub default_background: Option<String>,
}

impl TerminalColors {
    fn palette(&self, index: usize) -> Option<&str> {
        self.palette.get(index).and_then(|c| c.as_deref()).filter(|c| !c.is_empty())
    }
}

/// `terminalMode(colors)`: dark or light from the default background, if known.
pub fn terminal_mode(colors: &TerminalColors) -> Option<&'static str> {
    let bg = colors.default_background.as_deref().filter(|c| !c.is_empty())?;
    let bg = RGBA::from_hex(bg);
    Some(if 0.299 * bg.r() + 0.587 * bg.g() + 0.114 * bg.b() > 0.5 { "light" } else { "dark" })
}

/// `usablePalette(colors)` (context/theme.tsx): a background and a foreground are known.
pub fn usable_palette(colors: &TerminalColors) -> bool {
    // `(a ?? b)` then truthiness: an empty default color does not fall back to the palette.
    let known = |value: &Option<String>, index: usize| match value {
        Some(c) => !c.is_empty(),
        None => colors.palette(index).is_some(),
    };
    known(&colors.default_background, 0) && known(&colors.default_foreground, 7)
}

/// `generateSystem(colors, mode)`: a V1 theme source (`{ theme: { ... } }`) whose tokens are colors. Fails (a TS
/// `TypeError`) when neither a default color nor its palette entry is known, which `usablePalette` rules out.
pub fn generate_system(colors: &TerminalColors, mode: &str) -> Result<Value, ThemeError> {
    let or = |value: &Option<String>, index: usize| -> Result<String, ThemeError> {
        match value.as_deref() {
            Some(c) => Ok(c.to_owned()),
            None => colors
                .palette
                .get(index)
                .cloned()
                .flatten()
                .ok_or_else(|| ThemeError::new("null is not an object (evaluating 'hex.replace')")),
        }
    };
    let bg = RGBA::from_hex(&or(&colors.default_background, 0)?);
    let fg = RGBA::from_hex(&or(&colors.default_foreground, 7)?);
    let transparent = RGBA::from_values(bg.r(), bg.g(), bg.b(), 0.0);
    let is_dark = mode == "dark";
    let col = |index: usize| match colors.palette(index) {
        Some(value) => RGBA::from_hex(value),
        None => ansi_to_rgba(index as f64),
    };
    let grays = generate_gray_scale(bg, is_dark);
    let text_muted = generate_muted_text_color(bg, is_dark);
    let (red, green, yellow, blue, magenta, cyan, red_bright, green_bright) =
        (col(1), col(2), col(3), col(4), col(5), col(6), col(9), col(10));
    let diff_alpha = if is_dark { 0.22 } else { 0.14 };
    let diff_added_bg = tint(bg, green, diff_alpha);
    let diff_removed_bg = tint(bg, red, diff_alpha);
    let diff_context_bg = grays[2];
    let diff_added_line_number_bg = tint(diff_context_bg, green, diff_alpha);
    let diff_removed_line_number_bg = tint(diff_context_bg, red, diff_alpha);
    let tokens: Vec<(&str, RGBA)> = vec![
        ("primary", cyan),
        ("secondary", magenta),
        ("accent", cyan),
        ("error", red),
        ("warning", yellow),
        ("success", green),
        ("info", cyan),
        ("text", fg),
        ("textMuted", text_muted),
        ("selectedListItemText", bg),
        ("background", transparent),
        ("backgroundPanel", grays[2]),
        ("backgroundElement", grays[3]),
        ("backgroundMenu", grays[3]),
        ("borderSubtle", grays[6]),
        ("border", grays[7]),
        ("borderActive", grays[8]),
        ("diffAdded", green),
        ("diffRemoved", red),
        ("diffContext", grays[7]),
        ("diffHunkHeader", grays[7]),
        ("diffHighlightAdded", green_bright),
        ("diffHighlightRemoved", red_bright),
        ("diffAddedBg", diff_added_bg),
        ("diffRemovedBg", diff_removed_bg),
        ("diffContextBg", diff_context_bg),
        ("diffLineNumber", text_muted),
        ("diffAddedLineNumberBg", diff_added_line_number_bg),
        ("diffRemovedLineNumberBg", diff_removed_line_number_bg),
        ("markdownText", fg),
        ("markdownHeading", fg),
        ("markdownLink", blue),
        ("markdownLinkText", cyan),
        ("markdownCode", green),
        ("markdownBlockQuote", yellow),
        ("markdownEmph", yellow),
        ("markdownStrong", fg),
        ("markdownHorizontalRule", grays[7]),
        ("markdownListItem", blue),
        ("markdownListEnumeration", cyan),
        ("markdownImage", blue),
        ("markdownImageText", cyan),
        ("markdownCodeBlock", fg),
        ("syntaxComment", text_muted),
        ("syntaxKeyword", magenta),
        ("syntaxFunction", blue),
        ("syntaxVariable", fg),
        ("syntaxString", green),
        ("syntaxNumber", yellow),
        ("syntaxType", cyan),
        ("syntaxOperator", cyan),
        ("syntaxPunctuation", fg),
    ];
    let theme = Rec::from_entries(tokens.into_iter().map(|(k, c)| (k.to_owned(), Value::Color(Color::plain(c)))));
    let mut source = Rec::new();
    source.set("theme", Value::Rec(theme));
    Ok(Value::Rec(source))
}

/// Index 1..12.
fn generate_gray_scale(bg: RGBA, is_dark: bool) -> [RGBA; 13] {
    let mut grays = [RGBA::default(); 13];
    let (bg_r, bg_g, bg_b) = (bg.r() * 255.0, bg.g() * 255.0, bg.b() * 255.0);
    let luminance = 0.299 * bg_r + 0.587 * bg_g + 0.114 * bg_b;
    for (i, gray_slot) in grays.iter_mut().enumerate().skip(1) {
        let factor = i as f64 / 12.0;
        if is_dark && luminance < 10.0 {
            let gray = (factor * 0.4 * 255.0).floor();
            *gray_slot = RGBA::from_ints(gray, gray, gray, 255.0);
            continue;
        }
        if !is_dark && luminance > 245.0 {
            let gray = (255.0 - factor * 0.4 * 255.0).floor();
            *gray_slot = RGBA::from_ints(gray, gray, gray, 255.0);
            continue;
        }
        let next = if is_dark { luminance + (255.0 - luminance) * factor * 0.4 } else { luminance * (1.0 - factor * 0.4) };
        let ratio = next / luminance;
        let channel = |v: f64| super::js::min(super::js::max(v * ratio, 0.0), 255.0).floor();
        *gray_slot = RGBA::from_ints(channel(bg_r), channel(bg_g), channel(bg_b), 255.0);
    }
    grays
}

fn generate_muted_text_color(bg: RGBA, is_dark: bool) -> RGBA {
    let luminance = 0.299 * bg.r() * 255.0 + 0.587 * bg.g() * 255.0 + 0.114 * bg.b() * 255.0;
    if is_dark {
        let gray = if luminance < 10.0 { 180.0 } else { (160.0 + luminance * 0.3).floor().min(200.0) };
        return RGBA::from_ints(gray, gray, gray, 255.0);
    }
    let gray = if luminance > 245.0 { 75.0 } else { (100.0 - (255.0 - luminance) * 0.2).floor().max(60.0) };
    RGBA::from_ints(gray, gray, gray, 255.0)
}
