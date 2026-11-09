//! V1 themes (`{ defs, theme }`, the built-in theme files and the generated system theme) and their migration to
//! theme documents: `packages/theme/src/tui/v1.ts`, `v1-migrate.ts` and `v1-migrate-new-designs.ts` (the
//! "new-designs" experiment's layout).
//!
//! Divergence: where the TS reads a token a V1 theme lacks (`hex(undefined)`), it throws a JavaScriptCore
//! `TypeError`; here the error text is `undefined is not an object (evaluating '<token>')` (engine wording, which
//! the standard masks).

use super::color::{Oklch, oklch_to_hex, rgb_to_oklch};
use super::js;
use super::resolve::ThemeError;
use super::rgba::RGBA;
use super::schema::HUE_STEPS;
use super::value::{Rec, Value};

/// A resolved V1 theme (`Theme` of v1.ts): every token's color, by name.
#[derive(Clone, Debug)]
pub struct V1Theme {
    colors: Vec<(String, RGBA)>,
    pub has_selected_list_item_text: bool,
    pub thinking_opacity: f64,
}

impl V1Theme {
    pub fn get(&self, key: &str) -> Option<RGBA> {
        self.colors.iter().find(|(k, _)| k == key).map(|(_, c)| *c)
    }

    /// `theme[key]`, which must be there.
    fn c(&self, key: &str) -> Result<RGBA, ThemeError> {
        self.get(key).ok_or_else(|| ThemeError::new(format!("undefined is not an object (evaluating '{key}')")))
    }

    fn set(&mut self, key: &str, color: RGBA) {
        match self.colors.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = color,
            None => self.colors.push((key.to_owned(), color)),
        }
    }

    /// Every token, in the source's order (and `selectedListItemText`, `backgroundMenu` last when the source lacks
    /// them).
    pub fn colors(&self) -> &[(String, RGBA)] {
        &self.colors
    }
}

/// `resolveV1(theme, mode)`: every token of `theme.theme` resolved through `defs` and the theme itself.
pub fn resolve_v1(theme: &Rec, mode: &str) -> Result<V1Theme, ThemeError> {
    let defs = theme.get("defs").and_then(Value::as_rec).cloned().unwrap_or_default();
    // `Object.entries(theme.theme)`: a string's characters and an array's items are entries too.
    let tokens = match theme.get("theme") {
        Some(Value::Rec(rec)) => rec.clone(),
        Some(Value::Undefined | Value::Null) | None => {
            return Err(ThemeError::new("Object.entries requires that input parameter not be null or undefined"));
        }
        Some(Value::Str(text)) => Rec::from_entries(
            text.encode_utf16()
                .enumerate()
                .map(|(i, unit)| (i.to_string(), Value::Str(String::from_utf16_lossy(&[unit])))),
        ),
        Some(Value::Arr(items)) => Rec::from_entries(items.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone()))),
        Some(_) => Rec::new(),
    };
    fn resolve_color(value: &Value, chain: &[String], defs: &Rec, tokens: &Rec, mode: &str) -> Result<RGBA, ThemeError> {
        match value {
            Value::Color(c) => Ok(c.rgba),
            Value::Str(text) => {
                if text == "transparent" || text == "none" {
                    return Ok(RGBA::from_ints(0.0, 0.0, 0.0, 0.0));
                }
                if text.starts_with('#') {
                    return Ok(RGBA::from_hex(text));
                }
                if chain.iter().any(|c| c == text) {
                    let all: Vec<&str> = chain.iter().map(String::as_str).chain([text.as_str()]).collect();
                    return Err(ThemeError::new(format!("Circular color reference: {}", all.join(" -> "))));
                }
                let next = defs.get(text).filter(|v| !matches!(v, Value::Undefined | Value::Null)).or_else(|| tokens.get(text));
                let next = match next {
                    Some(v) if !v.is_undefined() => v,
                    // An inherited property (a function, or Object.prototype): not a color either way.
                    None if super::value::is_prototype_property(text) => {
                        return Err(ThemeError::new("Invalid V1 theme color"));
                    }
                    _ => return Err(ThemeError::new(format!("Color reference \"{text}\" not found in defs or theme"))),
                };
                let mut chain = chain.to_vec();
                chain.push(text.clone());
                resolve_color(next, &chain, defs, tokens, mode)
            }
            Value::Num(code) => Ok(ansi(*code)),
            Value::Rec(rec) if rec.get(mode).is_some() => resolve_color(rec.get(mode).unwrap(), chain, defs, tokens, mode),
            _ => Err(ThemeError::new("Invalid V1 theme color")),
        }
    }
    let mut resolved = V1Theme { colors: Vec::new(), has_selected_list_item_text: false, thinking_opacity: 0.6 };
    for (key, value) in tokens.entries() {
        if key == "selectedListItemText" || key == "backgroundMenu" || key == "thinkingOpacity" {
            continue;
        }
        resolved.colors.push((key.to_owned(), resolve_color(value, &[], &defs, &tokens, mode)?));
    }
    let selected = tokens.defined("selectedListItemText");
    resolved.has_selected_list_item_text = selected.is_some();
    match selected {
        Some(value) => {
            let color = resolve_color(value, &[], &defs, &tokens, mode)?;
            resolved.set("selectedListItemText", color);
        }
        // `resolved.background`, possibly undefined.
        None => match resolved.get("background") {
            Some(background) => resolved.set("selectedListItemText", background),
            None => resolved.colors.retain(|(k, _)| k != "selectedListItemText"),
        },
    }
    match tokens.get("backgroundMenu").filter(|v| v.truthy()) {
        Some(value) => {
            let color = resolve_color(value, &[], &defs, &tokens, mode)?;
            resolved.set("backgroundMenu", color);
        }
        None => match resolved.get("backgroundElement") {
            Some(element) => resolved.set("backgroundMenu", element),
            None => resolved.colors.retain(|(k, _)| k != "backgroundMenu"),
        },
    }
    resolved.thinking_opacity = match tokens.get("thinkingOpacity") {
        Some(Value::Num(n)) => *n,
        _ => 0.6,
    };
    Ok(resolved)
}

fn ansi(code: f64) -> RGBA {
    super::color::ansi_to_rgba(code)
}

pub(crate) const CHROMATIC_HUES: [&str; 7] = ["red", "orange", "yellow", "green", "cyan", "blue", "purple"];
pub(crate) const CATEGORICAL_TOKENS: [&str; 7] = ["secondary", "accent", "success", "warning", "primary", "error", "info"];
const MINIMUM_CHROMA: f64 = 0.03;
const LIGHT_THRESHOLD: f64 = 0.6;
const HUE_REFERENCES_LIGHT: [&str; 7] = ["#fca5a5", "#fdba74", "#fde047", "#86efac", "#67e8f9", "#93c5fd", "#d8b4fe"];
const HUE_REFERENCES_DARK: [&str; 7] = ["#b91c1c", "#c2410c", "#a16207", "#15803d", "#0e7490", "#1d4ed8", "#7e22ce"];

fn hue_angles(light: bool) -> [f64; 7] {
    let references = if light { HUE_REFERENCES_LIGHT } else { HUE_REFERENCES_DARK };
    references.map(|hex| to_oklch(RGBA::from_hex(hex)).h)
}

/// `toOklch(color)`.
pub(crate) fn to_oklch(color: RGBA) -> Oklch {
    let [r, g, b, _] = color.to_ints();
    rgb_to_oklch(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
}

/// `hex(color)`: `#rrggbb`, with `aa` unless opaque.
pub(crate) fn hex(color: RGBA) -> String {
    color.to_hex()
}

/// `byte(value)`.
pub(crate) fn byte(value: u8) -> String {
    format!("{value:02x}")
}

fn luminance(color: RGBA) -> f64 {
    0.299 * color.r() + 0.587 * color.g() + 0.114 * color.b()
}

/// `detectMode(theme)`.
pub(crate) fn detect_mode(theme: &V1Theme) -> Result<&'static str, ThemeError> {
    Ok(if luminance(theme.c("text")?) > luminance(theme.c("background")?) { "dark" } else { "light" })
}

/// `selectedForeground(theme, background)`.
pub(crate) fn selected_foreground(theme: &V1Theme, background: RGBA) -> Result<RGBA, ThemeError> {
    if theme.has_selected_list_item_text {
        return theme.c("selectedListItemText");
    }
    if theme.c("background")?.a() != 0.0 {
        return theme.c("background");
    }
    Ok(if 0.299 * background.r() + 0.587 * background.g() + 0.114 * background.b() > 0.5 {
        RGBA::from_ints(0.0, 0.0, 0.0, 255.0)
    } else {
        RGBA::from_ints(255.0, 255.0, 255.0, 255.0)
    })
}

/// `inferHues(theme)`: the chromatic hue nearest each V1 token, and the token color kept per hue.
pub(crate) struct Inferred {
    pub by_hue: Vec<(&'static str, RGBA, f64)>,
    pub by_token: Vec<(&'static str, &'static str)>,
}

impl Inferred {
    pub fn hue(&self, name: &str) -> Option<RGBA> {
        self.by_hue.iter().find(|(n, _, _)| *n == name).map(|(_, c, _)| *c)
    }

    pub fn token(&self, token: &str) -> Option<&'static str> {
        self.by_token.iter().find(|(t, _)| *t == token).map(|(_, h)| *h)
    }

    fn set_hue(&mut self, name: &'static str, color: RGBA, distance: f64) {
        match self.by_hue.iter_mut().find(|(n, _, _)| *n == name) {
            Some(slot) => *slot = (name, color, distance),
            None => self.by_hue.push((name, color, distance)),
        }
    }

    fn set_token(&mut self, token: &'static str, hue: &'static str) {
        match self.by_token.iter_mut().find(|(t, _)| *t == token) {
            Some(slot) => slot.1 = hue,
            None => self.by_token.push((token, hue)),
        }
    }
}

pub(crate) fn infer_hues(theme: &V1Theme) -> Result<Inferred, ThemeError> {
    let colors = [
        ("accent", theme.c("accent")?),
        ("success", theme.c("success")?),
        ("warning", theme.c("warning")?),
        ("primary", theme.c("primary")?),
        ("error", theme.c("error")?),
        ("info", theme.c("info")?),
        ("secondary", theme.c("secondary")?),
    ];
    let mut inferred = Inferred { by_hue: Vec::new(), by_token: Vec::new() };
    for (token, color) in colors {
        let Some((name, distance)) = infer_hue(color) else { continue };
        let current = inferred.by_hue.iter().find(|(n, _, _)| *n == name).map(|(_, _, d)| *d);
        if !current.is_some_and(|d| d <= distance) {
            inferred.set_hue(name, color, distance);
        }
        inferred.set_token(token, name);
    }
    for (token, color) in [("accent", theme.c("accent")?), ("primary", theme.c("primary")?)] {
        let Some((name, distance)) = infer_hue(color) else { continue };
        inferred.set_hue(name, color, distance);
        inferred.set_token(token, name);
    }
    Ok(inferred)
}

fn infer_hue(color: RGBA) -> Option<(&'static str, f64)> {
    let value = to_oklch(color);
    if ambiguous(color, value.c) {
        return None;
    }
    let reference = hue_angles(value.l >= LIGHT_THRESHOLD);
    let mut distances: Vec<(&'static str, f64)> =
        CHROMATIC_HUES.iter().zip(reference).map(|(name, angle)| (*name, hue_distance(value.h, angle))).collect();
    distances.sort_by(|a, b| js_compare(a.1 - b.1));
    distances.first().copied()
}

/// A JS sort comparator's result as an ordering (NaN compares equal, as JS treats it).
fn js_compare(difference: f64) -> std::cmp::Ordering {
    if difference < 0.0 {
        std::cmp::Ordering::Less
    } else if difference > 0.0 {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
}

fn hue_distance(first: f64, second: f64) -> f64 {
    let difference = (first - second).abs();
    js::min(difference, 360.0 - difference)
}

fn ambiguous(color: RGBA, chroma: f64) -> bool {
    color.to_ints()[3] == 0 || chroma < MINIMUM_CHROMA
}

/// `referenceHues(theme)`: every color string equal to a hue scale step becomes a `$hue.<name>.<step>` reference.
pub(crate) fn reference_hues(theme: Rec) -> Rec {
    let Some(definitions) = theme.get("hue").and_then(Value::as_rec).cloned() else { return theme };
    let mut scales: Vec<(String, Rec)> = Vec::new();
    fn resolve(definitions: &Rec, scales: &mut Vec<(String, Rec)>, name: &str, chain: &[String]) -> Option<Rec> {
        if let Some((_, cached)) = scales.iter().find(|(n, _)| n == name) {
            return Some(cached.clone());
        }
        if chain.iter().any(|c| c == name) {
            return None;
        }
        let value = definitions.get(name).filter(|v| v.truthy())?;
        let Value::Str(text) = value else {
            let scale = value.as_rec().cloned().unwrap_or_default();
            scales.push((name.to_owned(), scale.clone()));
            return Some(scale);
        };
        let target = text.strip_prefix("$hue.").filter(|t| !t.is_empty() && !t.contains('.'))?;
        let mut next = chain.to_vec();
        next.push(name.to_owned());
        let scale = resolve(definitions, scales, target, &next)?;
        scales.push((name.to_owned(), scale.clone()));
        Some(scale)
    }
    let mut references: Vec<(String, String)> = Vec::new();
    let mut index = |name: &str, overwrite: bool, scales: &mut Vec<(String, Rec)>| {
        let Some(scale) = resolve(&definitions, scales, name, &[]) else { return };
        for step in HUE_STEPS {
            let Some(color) = scale.get(&step.to_string()).filter(|v| v.truthy()).and_then(Value::as_str) else {
                continue;
            };
            let lower = color.to_lowercase();
            let existing = references.iter().position(|(k, _)| *k == lower);
            if !overwrite && existing.is_some() {
                continue;
            }
            let reference = format!("$hue.{name}.{step}");
            match existing {
                Some(i) => references[i].1 = reference,
                None => references.push((lower, reference)),
            }
        }
    };
    for name in CHROMATIC_HUES {
        index(name, false, &mut scales);
    }
    index("gray", false, &mut scales);
    index("accent", true, &mut scales);
    index("interactive", true, &mut scales);
    index("neutral", true, &mut scales);
    fn replace(value: &Value, references: &[(String, String)]) -> Value {
        match value {
            Value::Str(text) => {
                let lower = text.to_lowercase();
                references.iter().find(|(k, _)| *k == lower).map(|(_, r)| Value::Str(r.clone())).unwrap_or_else(|| value.clone())
            }
            Value::Rec(rec) => Value::Rec(Rec::from_entries(rec.entries().map(|(k, v)| (k.to_owned(), replace(v, references))))),
            _ => value.clone(),
        }
    }
    Rec::from_entries(theme.entries().map(|(key, value)| {
        let value = if key == "hue" || key == "categorical" { value.clone() } else { replace(value, &references) };
        (key.to_owned(), value)
    }))
}

/// `dialogBackground(theme)`: an opaque surface for dialogs.
pub(crate) fn dialog_background(theme: &V1Theme) -> Result<String, ThemeError> {
    if theme.c("backgroundPanel")?.a() == 1.0 {
        return Ok("$background.raised.base".into());
    }
    let candidates = [theme.c("backgroundPanel")?, theme.c("backgroundMenu")?, theme.c("backgroundElement")?, theme.c("background")?];
    let Some(visible) = candidates.into_iter().find(|c| c.a() > 0.0) else {
        return Ok(if luminance(theme.c("text")?) > 0.5 { "#000000".into() } else { "#ffffff".into() });
    };
    let [r, g, b, _] = visible.to_ints();
    Ok(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// `base(definition)`: the definition without its hues.
pub(crate) fn base(definition: &Rec) -> Rec {
    let mut out = definition.clone();
    out.remove("hue");
    out
}

/// Builds a record from literal entries.
pub(crate) fn rec<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Rec(Rec::from_entries(entries.into_iter().map(|(k, v)| (k.to_owned(), v))))
}

pub(crate) fn s(text: impl Into<String>) -> Value {
    Value::Str(text.into())
}

/// `migrateV1(theme)`.
pub fn migrate_v1(theme: &Rec) -> Result<Rec, ThemeError> {
    let light = resolve_v1(theme, "light")?;
    let dark = resolve_v1(theme, "dark")?;
    let (lb, db) = (light.c("background")?, dark.c("background")?);
    if lb.a() > 0.0 && db.a() > 0.0 && lb.equals(&db) {
        let light_mode = detect_mode(&light)?;
        let dark_mode = detect_mode(&dark)?;
        if light_mode == dark_mode {
            let definition = migrate_mode(if light_mode == "light" { &light } else { &dark }, light_mode)?;
            let hue = definition.get("hue").cloned().unwrap_or(Value::Undefined);
            let mut out = Rec::new();
            out.set("base", Value::Rec(base(&definition)));
            out.set(light_mode, rec([("hue", hue)]));
            return Ok(out);
        }
    }
    let light_definition = migrate_mode(&light, "light")?;
    let dark_definition = migrate_mode(&dark, "dark")?;
    let mut out = Rec::new();
    out.set("base", Value::Rec(base(&light_definition)));
    out.set("light", rec([("hue", light_definition.get("hue").cloned().unwrap_or(Value::Undefined))]));
    out.set("dark", Value::Rec(dark_definition));
    Ok(out)
}

/// `{ syntax: ..., markdown: ... }` and the other V1-colored token groups (`paletteTokens`, shared by both
/// migrations).
pub(crate) fn palette_tokens(theme: &V1Theme) -> Result<Vec<(&'static str, Value)>, ThemeError> {
    let color = |key: &str| theme.c(key).map(|c| s(hex(c)));
    Ok(vec![
        ("border", rec([("base", color("border")?)])),
        ("scrollbar", rec([("base", color("borderActive")?)])),
        (
            "diff",
            rec([
                (
                    "text",
                    rec([
                        ("added", color("diffAdded")?),
                        ("removed", color("diffRemoved")?),
                        ("context", color("diffContext")?),
                        ("hunkHeader", color("diffHunkHeader")?),
                    ]),
                ),
                (
                    "background",
                    rec([
                        ("added", color("diffAddedBg")?),
                        ("removed", color("diffRemovedBg")?),
                        ("context", color("diffContextBg")?),
                    ]),
                ),
                ("highlight", rec([("added", color("diffHighlightAdded")?), ("removed", color("diffHighlightRemoved")?)])),
                (
                    "lineNumber",
                    rec([
                        ("text", color("diffLineNumber")?),
                        (
                            "background",
                            rec([("added", color("diffAddedLineNumberBg")?), ("removed", color("diffRemovedLineNumberBg")?)]),
                        ),
                    ]),
                ),
            ]),
        ),
        (
            "syntax",
            rec([
                ("comment", color("syntaxComment")?),
                ("keyword", color("syntaxKeyword")?),
                ("function", color("syntaxFunction")?),
                ("variable", color("syntaxVariable")?),
                ("string", color("syntaxString")?),
                ("number", color("syntaxNumber")?),
                ("type", color("syntaxType")?),
                ("operator", color("syntaxOperator")?),
                ("punctuation", color("syntaxPunctuation")?),
            ]),
        ),
        (
            "markdown",
            rec([
                ("text", color("markdownText")?),
                ("heading", color("markdownHeading")?),
                ("link", color("markdownLink")?),
                ("linkText", color("markdownLinkText")?),
                ("code", color("markdownCode")?),
                ("blockQuote", color("markdownBlockQuote")?),
                ("emphasis", color("markdownEmph")?),
                ("strong", color("markdownStrong")?),
                ("horizontalRule", color("markdownHorizontalRule")?),
                ("listItem", color("markdownListItem")?),
                ("listEnumeration", color("markdownListEnumeration")?),
                ("image", color("markdownImage")?),
                ("imageText", color("markdownImageText")?),
                ("codeBlock", color("markdownCodeBlock")?),
            ]),
        ),
    ])
}

/// The `hue` and `categorical` of a migrated mode (`paletteHues` of the new-designs migration; the same layout in
/// `migrateMode`).
pub(crate) fn palette_hues(
    theme: &V1Theme,
    gray: Rec,
    scale: &dyn Fn(RGBA) -> Result<Rec, ThemeError>,
) -> Result<(Value, Value), ThemeError> {
    let hues = infer_hues(theme)?;
    let mut categorical: Vec<&str> = Vec::new();
    for token in CATEGORICAL_TOKENS {
        if let Some(hue) = hues.token(token) {
            categorical.push(hue);
        }
    }
    let mut unique: Vec<&str> = Vec::new();
    for hue in categorical {
        if !unique.contains(&hue) {
            unique.push(hue);
        }
    }
    let mut hue = Rec::new();
    hue.set("gray", Value::Rec(gray));
    for name in CHROMATIC_HUES {
        if let Some(color) = hues.hue(name) {
            hue.set(name, Value::Rec(scale(color)?));
        }
    }
    hue.set("accent", s(hues.token("accent").map(|h| format!("$hue.{h}")).unwrap_or_else(|| "$hue.gray".into())));
    hue.set("interactive", s(hues.token("primary").map(|h| format!("$hue.{h}")).unwrap_or_else(|| "$hue.gray".into())));
    hue.set("neutral", s("$hue.gray"));
    let categorical = if unique.is_empty() { vec![s("neutral")] } else { unique.into_iter().map(s).collect() };
    Ok((Value::Rec(hue), Value::Arr(categorical)))
}

fn migrate_mode(theme: &V1Theme, mode: &str) -> Result<Rec, ThemeError> {
    let color = |key: &str| theme.c(key).map(|c| s(hex(c)));
    let selected = hex(selected_foreground(theme, theme.c("primary")?)?);
    let destructive = hex(selected_foreground(theme, theme.c("error")?)?);
    let (hue, categorical) = palette_hues(theme, neutral_scale(theme)?, &|c| Ok(hue_scale(c, mode)))?;
    let text = "$hue.neutral.200";
    let text_muted = "$hue.neutral.400";
    let primary = "$hue.interactive.200";
    let background = "$hue.neutral.800";
    let background_panel = "$hue.neutral.700";
    let background_menu = "$hue.neutral.600";
    let background_raised_max = "$hue.neutral.500";
    let mut definition = Rec::new();
    definition.set("hue", hue);
    definition.set("categorical", categorical);
    definition.set(
        "text",
        rec([
            ("base", s(text)),
            ("muted", s(text_muted)),
            (
                "action",
                rec([
                    (
                        "primary",
                        rec([("base", s("$text.base")), ("$disabled", s(text_muted)), ("$focused", s(selected)), ("$selected", s(primary))]),
                    ),
                    ("secondary", rec([("base", s("$text.muted")), ("$hovered", s("$text.base"))])),
                    ("destructive", rec([("base", s(destructive)), ("$disabled", s(text_muted))])),
                ]),
            ),
            (
                "formfield",
                rec([
                    ("base", s(text)),
                    ("$hovered", s(primary)),
                    ("$focused", s(primary)),
                    ("$pressed", s(primary)),
                    ("$disabled", s(text_muted)),
                    ("$selected", s(primary)),
                ]),
            ),
            (
                "feedback",
                rec([
                    ("error", rec([("base", color("error")?)])),
                    ("warning", rec([("base", color("warning")?)])),
                    ("success", rec([("base", color("success")?)])),
                    ("info", rec([("base", color("info")?)])),
                ]),
            ),
        ]),
    );
    definition.set(
        "background",
        rec([
            ("base", s(background)),
            ("raised", rec([("base", s(background_panel)), ("high", s(background_menu)), ("max", s(background_raised_max))])),
            (
                "action",
                rec([
                    (
                        "primary",
                        rec([
                            ("base", s("transparent")),
                            ("$hovered", s(background_panel)),
                            ("$focused", s(primary)),
                            ("$selected", s("transparent")),
                        ]),
                    ),
                    ("secondary", rec([("base", s("transparent"))])),
                    ("destructive", rec([("base", color("error")?)])),
                ]),
            ),
            ("formfield", rec([("base", s("$background.base"))])),
            (
                "feedback",
                rec([
                    ("error", rec([("base", s("$background.base"))])),
                    ("warning", rec([("base", s("$background.base"))])),
                    ("success", rec([("base", s("$background.base"))])),
                    ("info", rec([("base", s("$background.base"))])),
                ]),
            ),
        ]),
    );
    for (key, value) in palette_tokens(theme)? {
        definition.set(key, value);
    }
    definition.set(
        "@dialog",
        rec([(
            "background",
            rec([
                ("base", s(dialog_background(theme)?)),
                ("action", rec([("primary", rec([("$hovered", s("$background.raised.high"))]))])),
            ]),
        )]),
    );
    Ok(reference_hues(definition))
}

fn hue_scale(color: RGBA, mode: &str) -> Rec {
    let value = to_oklch(color);
    let anchor = 200.0;
    let endpoint = if mode == "light" { js::max(0.97, value.l) } else { js::min(0.18, value.l) };
    let alpha = color.to_ints()[3];
    Rec::from_entries(HUE_STEPS.iter().map(|step| {
        let hex_value = if *step == 200 {
            hex(color)
        } else {
            let progress = (*step as f64 - anchor) / (900.0 - anchor);
            let generated = oklch_to_hex(Oklch {
                l: value.l + (endpoint - value.l) * progress,
                c: value.c * (1.0 - progress * 0.5),
                h: value.h,
            });
            if alpha == 255 { generated } else { format!("{generated}{}", byte(alpha)) }
        };
        (step.to_string(), s(hex_value))
    }))
}

fn neutral_scale(theme: &V1Theme) -> Result<Rec, ThemeError> {
    // neutralAnchors: the light-mode layout, reversed and mirrored (step -> 1000 - step).
    let anchors: [(u16, RGBA); 5] = [
        (200, theme.c("text")?),
        (400, theme.c("textMuted")?),
        // `theme.backgroundElement || theme.backgroundMenu`
        (600, theme.get("backgroundElement").map_or_else(|| theme.c("backgroundMenu"), Ok)?),
        (700, theme.c("backgroundPanel")?),
        (800, theme.c("background")?),
    ];
    Ok(Rec::from_entries(HUE_STEPS.iter().map(|step| {
        if let Some((_, color)) = anchors.iter().find(|(s, _)| s == step) {
            return (step.to_string(), s(hex(*color)));
        }
        let first = anchors[0];
        let last = anchors[anchors.len() - 1];
        let (lower, upper) = if *step < first.0 {
            (first, anchors[1])
        } else if *step > last.0 {
            (anchors[anchors.len() - 2], last)
        } else {
            (*anchors.iter().rfind(|(s, _)| s < step).unwrap(), *anchors.iter().find(|(s, _)| s > step).unwrap())
        };
        let amount = (*step as f64 - lower.0 as f64) / (upper.0 as f64 - lower.0 as f64);
        (step.to_string(), s(interpolate(lower.1, upper.1, amount)))
    })))
}

fn interpolate(first: RGBA, second: RGBA, amount: f64) -> String {
    let start = to_oklch(first);
    let end = to_oklch(second);
    let start_hue = if start.h.is_finite() {
        start.h
    } else if end.h.is_finite() {
        end.h
    } else {
        0.0
    };
    let end_hue = if end.h.is_finite() { end.h } else { start_hue };
    let hue = ((((end_hue - start_hue) % 360.0) + 540.0) % 360.0) - 180.0;
    let generated = oklch_to_hex(Oklch {
        l: start.l + (end.l - start.l) * amount,
        c: start.c + (end.c - start.c) * amount,
        h: start_hue + hue * amount,
    });
    let a1 = first.to_ints()[3] as f64;
    let a2 = second.to_ints()[3] as f64;
    let alpha = js::max(0.0, js::min(255.0, js::round(a1 + (a2 - a1) * amount)));
    if alpha == 255.0 { generated } else { format!("{generated}{}", byte(alpha as u8)) }
}

/// The new-designs migration (`v1-migrate-new-designs.ts`).
pub mod new_designs {
    use super::*;

    struct Profile {
        surface: [(u16, f64); 6],
        text: [(u16, f64); 2],
        hue_lightness: [(u16, f64); 7],
        hue_chroma: [(u16, f64); 8],
        hue_mid: f64,
        readable: [(u16, f64); 5],
        readable_hue: [(u16, f64); 4],
    }

    fn lookup(table: &[(u16, f64)], step: u16) -> f64 {
        table.iter().find(|(s, _)| *s == step).map(|(_, v)| *v).unwrap_or(f64::NAN)
    }

    const DARK: Profile = Profile {
        surface: [(900, 0.0), (800, 0.039), (700, 0.057), (600, 0.11), (500, 0.175), (400, 0.253)],
        text: [(200, 0.719), (300, 0.538)],
        hue_lightness: [(100, -0.035), (200, -0.023), (300, -0.003), (600, 0.095), (700, 0.071), (800, 0.024), (900, 0.012)],
        hue_chroma: [(100, 0.1), (200, 0.24), (300, 0.22), (500, 0.64), (600, 0.2), (700, 0.14), (800, 0.09), (900, 0.07)],
        hue_mid: 0.55,
        readable: [(800, 0.95), (700, 0.92), (600, 0.81), (500, 0.66), (900, 0.99)],
        readable_hue: [(600, 0.7), (700, 0.82), (800, 0.92), (900, 0.99)],
    };

    const LIGHT: Profile = Profile {
        surface: [(900, 0.0), (800, -0.015), (700, -0.039), (600, -0.102), (500, -0.145), (400, -0.227)],
        text: [(200, 0.688), (300, 0.484)],
        hue_lightness: [(100, 0.083), (200, 0.13), (300, 0.116), (600, 0.021), (700, -0.002), (800, -0.004), (900, -0.008)],
        hue_chroma: [(100, 0.19), (200, 0.12), (300, 0.16), (500, 0.38), (600, 0.17), (700, 0.1), (800, 0.05), (900, 0.02)],
        hue_mid: 0.65,
        readable: [(800, 0.96), (700, 0.89), (600, 0.74), (500, 0.64), (900, 0.97)],
        readable_hue: [(600, 0.78), (700, 0.88), (800, 0.93), (900, 0.97)],
    };

    const MINIMUM_SURFACE_CONTRAST: f64 = 4.5;
    const MAXIMUM_DIMMING: f64 = 0.6;

    fn profile(mode: &str) -> &'static Profile {
        if mode == "dark" { &DARK } else { &LIGHT }
    }

    fn contrast(first: RGBA, second: RGBA) -> f64 {
        let channel = |value: f64| if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) };
        let luminance = |c: RGBA| 0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b());
        let (a, b) = (luminance(first), luminance(second));
        // [a, b].sort((a, b) => b - a)
        let (lighter, darker) = if b - a > 0.0 { (b, a) } else { (a, b) };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn mix(start: Oklch, end: Oklch, amount: f64) -> Oklch {
        let start_hue = if start.c > 0.002 { start.h } else { end.h };
        let end_hue = if end.c > 0.002 { end.h } else { start_hue };
        let hue = ((((end_hue - start_hue) % 360.0) + 540.0) % 360.0) - 180.0;
        Oklch {
            l: start.l + (end.l - start.l) * amount,
            c: start.c + (end.c - start.c) * amount,
            h: start_hue + hue * amount,
        }
    }

    struct Neutral {
        scale: Rec,
        /// `{ ...lightness, ...planned }`.
        lightness: Vec<(u16, f64)>,
        text: RGBA,
        page: Oklch,
        page_contrast: f64,
    }

    impl Neutral {
        fn lightness(&self, step: u16) -> f64 {
            lookup(&self.lightness, step)
        }

        fn readable(&self, color: Oklch, share: f64) -> Oklch {
            readable(self.text, self.page, self.page_contrast, color, share)
        }
    }

    fn readable(text: RGBA, page: Oklch, page_contrast: f64, color: Oklch, share: f64) -> Oklch {
        let target = js::max(page_contrast * share, js::min(MINIMUM_SURFACE_CONTRAST, page_contrast * 0.95));
        let passes = |amount: f64| contrast(text, RGBA::from_hex(&oklch_to_hex(mix(color, page, amount)))) >= target;
        if passes(0.0) {
            return color;
        }
        if !passes(MAXIMUM_DIMMING) {
            return mix(color, page, MAXIMUM_DIMMING);
        }
        let (mut low, mut high) = (0.0, MAXIMUM_DIMMING);
        for _ in 0..12 {
            let amount = (low + high) / 2.0;
            if passes(amount) {
                high = amount
            } else {
                low = amount
            }
        }
        let _ = low;
        mix(color, page, high)
    }

    fn set(table: &mut Vec<(u16, f64)>, step: u16, value: f64) {
        match table.iter_mut().find(|(s, _)| *s == step) {
            Some(slot) => slot.1 = value,
            None => table.push((step, value)),
        }
    }

    fn neutral_scale(theme: &V1Theme, mode: &str) -> Result<Neutral, ThemeError> {
        let profile = profile(mode);
        let opaque = |c: RGBA| c.to_ints()[3] > 0;
        let background = theme.c("background")?;
        let panel = theme.c("backgroundPanel")?;
        let element_raw = theme.c("backgroundElement")?;
        let element = [element_raw, theme.c("backgroundMenu")?].into_iter().find(|c| opaque(*c));
        let sources: [(u16, Option<RGBA>); 3] = [(900, Some(background)), (700, Some(panel)), (600, element)];
        let reference = sources.iter().find(|(_, c)| c.is_some_and(opaque));
        let page = match reference {
            Some((_, Some(c))) => to_oklch(*c),
            _ => to_oklch(if mode == "dark" { RGBA::from_ints(0.0, 0.0, 0.0, 255.0) } else { RGBA::from_ints(255.0, 255.0, 255.0, 255.0) }),
        };
        let page_lightness = page.l - reference.map(|(step, _)| lookup(&profile.surface, *step)).unwrap_or(0.0);
        let tint = |colors: &[Option<RGBA>]| colors.iter().flatten().find(|c| opaque(**c)).map(|c| to_oklch(*c)).unwrap_or(page);
        let surface_tint = |step: u16| match step {
            900 => tint(&[Some(background), Some(panel), element]),
            800 | 700 => tint(&[Some(panel), Some(background), element]),
            _ => tint(&[element, Some(panel), Some(background)]),
        };
        let text_color = theme.c("text")?;
        let text = to_oklch(text_color);
        let page_color = Oklch { l: page_lightness, ..surface_tint(900) };
        let page_contrast = contrast(text_color, RGBA::from_hex(&oklch_to_hex(page_color)));
        let mut lightness: Vec<(u16, f64)> = Vec::new();
        let mut planned: Vec<(u16, f64)> = Vec::new();
        let mut scale = Rec::new();
        for step in [800u16, 700, 600, 500, 400] {
            let generated = Oklch { l: page_lightness + lookup(&profile.surface, step), ..surface_tint(step) };
            let color = if step == 400 {
                generated
            } else {
                readable(text_color, page_color, page_contrast, generated, lookup(&profile.readable, step))
            };
            set(&mut planned, step, generated.l);
            set(&mut lightness, step, color.l);
            scale.set(step.to_string(), s(oklch_to_hex(color)));
        }
        set(&mut lightness, 900, page_lightness);
        scale.set("900", s(hex(background)));
        for (step, color) in [(800u16, panel), (700, panel), (600, element_raw)] {
            if !opaque(color) {
                scale.set(step.to_string(), s(hex(color)));
            }
        }
        set(&mut lightness, 100, text.l);
        scale.set("100", s(hex(text_color)));
        for step in [200u16, 300] {
            let generated = mix(page_color, text, lookup(&profile.text, step));
            set(&mut lightness, step, generated.l);
            scale.set(step.to_string(), s(oklch_to_hex(generated)));
        }
        let muted_color = theme.c("textMuted")?;
        let muted = to_oklch(muted_color);
        let muted_step = if (muted.l - lookup(&lightness, 200)).abs() <= (muted.l - lookup(&lightness, 300)).abs() { 200 } else { 300 };
        set(&mut lightness, muted_step, muted.l);
        scale.set(muted_step.to_string(), s(hex(muted_color)));
        for (step, value) in planned {
            set(&mut lightness, step, value);
        }
        Ok(Neutral { scale, lightness, text: text_color, page: page_color, page_contrast })
    }

    fn hue_scale(color: RGBA, mode: &str, neutral: &Neutral) -> Rec {
        let profile = profile(mode);
        let anchor = to_oklch(color);
        let alpha = color.to_ints()[3];
        let generate = |generated: Oklch| {
            let value = oklch_to_hex(generated);
            if alpha == 255 { value } else { format!("{value}{}", byte(alpha)) }
        };
        let lightness = |step: u16| neutral.lightness(step) + lookup(&profile.hue_lightness, step);
        Rec::from_entries(HUE_STEPS.iter().map(|step| {
            let step = *step;
            if step == 400 {
                return (step.to_string(), s(hex(color)));
            }
            let generated = Oklch {
                l: if step == 500 { anchor.l + (lightness(600) - anchor.l) * profile.hue_mid } else { lightness(step) },
                c: anchor.c * lookup(&profile.hue_chroma, step),
                h: anchor.h,
            };
            let value = if matches!(step, 600 | 700 | 800 | 900) {
                generate(neutral.readable(generated, lookup(&profile.readable_hue, step)))
            } else {
                generate(generated)
            };
            (step.to_string(), s(value))
        }))
    }

    fn light_dialog() -> Value {
        rec([(
            "background",
            rec([
                ("base", s("$hue.neutral.800")),
                ("raised", rec([("high", s("$hue.neutral.700"))])),
                (
                    "action",
                    rec([
                        ("primary", rec([("$hovered", s("$hue.neutral.600"))])),
                        ("secondary", rec([("$hovered", s("$hue.neutral.600"))])),
                    ]),
                ),
            ]),
        )])
    }

    fn light_overrides(definition: &Rec, theme: &V1Theme) -> Result<Value, ThemeError> {
        let hue = definition.get("hue").cloned().unwrap_or(Value::Undefined);
        if theme.c("backgroundPanel")?.a() == 1.0 {
            return Ok(rec([("hue", hue), ("@dialog", light_dialog())]));
        }
        let mut dialog = light_dialog().as_rec().cloned().unwrap();
        let mut background = dialog.get("background").and_then(Value::as_rec).cloned().unwrap();
        background.set("base", s(dialog_background(theme)?));
        dialog.set("background", Value::Rec(background));
        Ok(rec([("hue", hue), ("@dialog", Value::Rec(dialog))]))
    }

    fn migrate_mode(theme: &V1Theme, mode: &str) -> Result<Rec, ThemeError> {
        let color = |key: &str| theme.c(key).map(|c| s(hex(c)));
        let neutral = neutral_scale(theme, mode)?;
        let primary = "$hue.interactive.400";
        let (hue, categorical) = palette_hues(theme, neutral.scale.clone(), &|c| Ok(hue_scale(c, mode, &neutral)))?;
        let mut definition = Rec::new();
        definition.set("hue", hue);
        definition.set("categorical", categorical);
        definition.set(
            "text",
            rec([
                ("base", s("$hue.neutral.100")),
                ("muted", s("$hue.neutral.200")),
                ("faint", s("$hue.neutral.300")),
                (
                    "action",
                    rec([
                        (
                            "primary",
                            rec([("base", s("$text.base")), ("$disabled", s("$text.faint")), ("$focused", s("$text.base")), ("$selected", s(primary))]),
                        ),
                        (
                            "secondary",
                            rec([("base", s("$text.muted")), ("$hovered", s("$text.base")), ("$focused", s("$hue.interactive.200"))]),
                        ),
                        (
                            "destructive",
                            rec([("base", s(hex(selected_foreground(theme, theme.c("error")?)?))), ("$disabled", s("$text.faint"))]),
                        ),
                    ]),
                ),
                (
                    "formfield",
                    rec([
                        ("base", s("$text.base")),
                        ("$hovered", s(primary)),
                        ("$focused", s(primary)),
                        ("$pressed", s(primary)),
                        ("$disabled", s("$text.faint")),
                        ("$selected", s(primary)),
                    ]),
                ),
                (
                    "feedback",
                    rec([
                        ("error", rec([("base", color("error")?)])),
                        ("warning", rec([("base", color("warning")?)])),
                        ("success", rec([("base", color("success")?)])),
                        ("info", rec([("base", color("info")?)])),
                    ]),
                ),
            ]),
        );
        definition.set(
            "background",
            rec([
                ("base", s("$hue.neutral.900")),
                ("raised", rec([("base", s("$hue.neutral.700")), ("high", s("$hue.neutral.600")), ("max", s("$hue.neutral.500"))])),
                (
                    "action",
                    rec([
                        (
                            "primary",
                            rec([
                                ("base", s("transparent")),
                                ("$hovered", s("$hue.neutral.700")),
                                ("$focused", s("$hue.interactive.600")),
                                ("$selected", s("transparent")),
                            ]),
                        ),
                        ("secondary", rec([("base", s("transparent"))])),
                        ("destructive", rec([("base", color("error")?)])),
                    ]),
                ),
                ("formfield", rec([("base", s("$background.base"))])),
                (
                    "feedback",
                    rec([
                        ("error", rec([("base", s("$background.base"))])),
                        ("warning", rec([("base", s("$background.base"))])),
                        ("success", rec([("base", s("$background.base"))])),
                        ("info", rec([("base", s("$background.base"))])),
                    ]),
                ),
            ]),
        );
        for (key, value) in palette_tokens(theme)? {
            definition.set(key, value);
        }
        definition.set(
            "@dialog",
            rec([(
                "background",
                rec([
                    ("base", s(dialog_background(theme)?)),
                    (
                        "action",
                        rec([
                            ("primary", rec([("$hovered", s("$background.raised.max"))])),
                            ("secondary", rec([("$hovered", s("$background.raised.max"))])),
                        ]),
                    ),
                ]),
            )]),
        );
        Ok(reference_hues(definition))
    }

    /// `migrateV1NewDesigns(theme)`.
    pub fn migrate_v1_new_designs(theme: &Rec) -> Result<Rec, ThemeError> {
        let light = resolve_v1(theme, "light")?;
        let dark = resolve_v1(theme, "dark")?;
        let (lb, db) = (light.c("background")?, dark.c("background")?);
        if lb.a() > 0.0 && db.a() > 0.0 && lb.equals(&db) {
            let light_mode = detect_mode(&light)?;
            let dark_mode = detect_mode(&dark)?;
            if light_mode == dark_mode {
                let source = if light_mode == "light" { &light } else { &dark };
                let definition = migrate_mode(source, light_mode)?;
                let mut out = Rec::new();
                out.set("base", Value::Rec(base(&definition)));
                if light_mode == "light" {
                    out.set("light", light_overrides(&definition, &light)?);
                } else {
                    out.set("dark", rec([("hue", definition.get("hue").cloned().unwrap_or(Value::Undefined))]));
                }
                return Ok(out);
            }
        }
        let light_definition = migrate_mode(&light, "light")?;
        let dark_definition = migrate_mode(&dark, "dark")?;
        let mut out = Rec::new();
        out.set("base", Value::Rec(base(&light_definition)));
        out.set("light", light_overrides(&light_definition, &light)?);
        out.set("dark", Value::Rec(dark_definition));
        Ok(out)
    }
}
