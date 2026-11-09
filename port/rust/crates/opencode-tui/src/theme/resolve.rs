//! `packages/theme/src/tui/resolve.ts`: decoding a theme document and resolving one of its modes to colors.
//!
//! Divergences: a reference to a property every JS object inherits (`$text.constructor`, a hue named `toString`)
//! finds that property in TS (a function, or `Object.prototype` for `__proto__`); here it fails like a missing
//! reference. Either way the document is rejected, with the same `Invalid theme ... value ...` text for decoded
//! documents. TODO(port): a categorical hue named after such a property resolves in TS (to a function).

use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use regex::Regex;
use std::sync::LazyLock;

use super::expand::expand_theme;
use super::js;
use super::resolved::{HueScale, ResolvedTheme, Views, tokens_of};
use super::rgba::RGBA;
use super::schema::{self, ACTION_STATES, ACTION_VARIANTS, HUE_STEPS, THEME_DOCUMENT, is_hex};
use super::select::{Mode, select_theme_mode, theme_modes};
use super::value::{Color, Origin, Rec, Value, is_prototype_property, merge_theme};

/// A theme that failed to decode or resolve: the TS `Error` and its message.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeError {
    pub message: String,
}

impl ThemeError {
    pub fn new(message: impl Into<String>) -> ThemeError {
        ThemeError { message: message.into() }
    }
}

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ThemeError {}

/// What a failure looks like as text to `themeDecodeError`: a schema error's message, or `String(error)`.
enum Failure {
    Schema(String),
    Error(ThemeError),
}

/// `themeDecodeError(error, name)`: `Invalid theme: <name> <value> is an invalid value`, the value taken from the
/// first `got <input>` of the message.
fn theme_decode_error(failure: Failure, name: &str) -> ThemeError {
    let text = match failure {
        Failure::Schema(message) => message,
        Failure::Error(error) => format!("Error: {}", error.message),
    };
    let value = js::got(&text).unwrap_or("value");
    ThemeError::new(format!("Invalid theme: {name} {value} is an invalid value"))
}

/// `parseThemeDocument(input, name = "theme")`: the decoded document (schema keys only), after checking that every
/// mode it defines resolves.
pub fn parse_theme_document(input: &Value, name: &str) -> Result<Rec, ThemeError> {
    let document = THEME_DOCUMENT.decode(input).map_err(|issues| theme_decode_error(Failure::Schema(schema::message(&issues)), name))?;
    let Value::Rec(document) = document else { unreachable!("a struct decodes to a record") };
    for mode in theme_modes(&document) {
        resolve_theme_document(&document, Some(mode)).map_err(|error| theme_decode_error(Failure::Error(error), name))?;
    }
    Ok(document)
}

/// `resolveThemeDocument(document, mode)`.
pub fn resolve_theme_document(document: &Rec, mode: Option<Mode>) -> Result<ResolvedTheme, ThemeError> {
    let (theme, _) = select_theme_mode(document, mode).map_err(ThemeError::new)?;
    resolve_expanded_theme(&expand_theme(&theme))
}

static NEXT_THEME: AtomicU32 = AtomicU32::new(1);

/// The resolved hue record: names in property order and their scales.
pub(crate) struct HueTable {
    pub names: Vec<String>,
    pub scales: Vec<HueScale>,
}

impl HueTable {
    pub fn get(&self, name: &str) -> Option<&HueScale> {
        self.names.iter().position(|n| n == name).map(|i| &self.scales[i])
    }

    /// The hue record as the resolver's `source.hue`.
    fn value(&self) -> Value {
        Value::Rec(Rec::from_entries(self.names.iter().zip(&self.scales).map(|(name, scale)| {
            (
                name.clone(),
                Value::Rec(Rec::from_entries(
                    HUE_STEPS.iter().zip(scale.0.iter()).map(|(step, color)| (step.to_string(), Value::Color(*color))),
                )),
            )
        })))
    }
}

fn resolve_expanded_theme(definition: &Rec) -> Result<ResolvedTheme, ThemeError> {
    let theme = NEXT_THEME.fetch_add(1, Ordering::Relaxed);
    let hue = resolve_hue(definition.get("hue").unwrap_or(&Value::Undefined), theme)?;
    let mut categorical = Vec::new();
    for name in definition.get("categorical").and_then(arr).unwrap_or(&[]) {
        let name = name.as_str().unwrap_or_default();
        let scale = hue.get(name).ok_or_else(|| ThemeError::new(format!("Categorical hue \"{name}\" was not found")))?;
        categorical.push(*scale);
    }
    let base = tokens(definition);
    let hue_value = hue.value();
    let view = |tokens: &Rec| resolve_view(tokens, &hue_value).map(|resolved| tokens_of(&resolved, &hue, &categorical));
    let dialog = definition.get("@dialog").filter(|d| d.truthy());
    let dialog_tokens = match dialog {
        Some(dialog) => contextualize(&base, &tokens(dialog.as_rec().unwrap_or(&Rec::new()))),
        None => base.clone(),
    };
    let dialog_view = view(&dialog_tokens)?;
    let footer = dialog.map(|d| d.at("@footer")).filter(|f| f.truthy());
    let footer_view = match footer {
        Some(footer) => Some(view(&contextualize(&dialog_tokens, footer.as_rec().unwrap_or(&Rec::new())))?),
        None => None,
    };
    let base_view = view(&base)?;
    Ok(ResolvedTheme::new(Rc::new(Views {
        base: base_view,
        dialog: dialog_view,
        footer: footer_view,
        hue,
        theme,
    })))
}

fn arr(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Arr(items) => Some(items),
        _ => None,
    }
}

/// `tokens(definition)`: the seven token groups (missing ones `undefined`).
fn tokens(definition: &Rec) -> Rec {
    let mut out = Rec::new();
    for key in ["text", "background", "border", "scrollbar", "diff", "syntax", "markdown"] {
        out.set(key, definition.get(key).cloned().unwrap_or(Value::Undefined));
    }
    out
}

fn contextualize(base: &Rec, overlay: &Rec) -> Rec {
    let base_value = Value::Rec(base.clone());
    let overlay_value = Value::Rec(overlay.clone());
    let mut result = merge_theme(&[&base_value, &overlay_value]);
    for group in ["text", "background"] {
        let mut merged = result.get(group).and_then(Value::as_rec).cloned().unwrap_or_default();
        merged.set(
            "action",
            contextual_actions(base_value.at(group).at("action"), overlay_value.at(group).at("action")),
        );
        result.set(group, Value::Rec(merged));
    }
    result
}

/// `a ?? b` over optional values.
fn first<'a>(values: impl IntoIterator<Item = &'a Value>) -> Value {
    values.into_iter().find(|v| !matches!(v, Value::Undefined | Value::Null)).cloned().unwrap_or(Value::Undefined)
}

fn contextual_actions(base: &Value, surface: &Value) -> Value {
    let mut out = Rec::new();
    for variant in ACTION_VARIANTS {
        let base_variant = base.at(variant);
        let surface_variant = surface.at(variant);
        let mut states = Rec::new();
        states.set("base", first([surface_variant.at("base"), base_variant.at("base")]));
        for state in ACTION_STATES {
            let key = format!("${state}");
            states.set(
                key.clone(),
                first([surface_variant.at(&key), surface_variant.at("base"), base_variant.at(&key), base_variant.at("base")]),
            );
        }
        out.set(variant, Value::Rec(states));
    }
    Value::Rec(out)
}

fn resolve_view(definition: &Rec, hue: &Value) -> Result<Value, ThemeError> {
    let mut source = Rec::new();
    source.set("hue", hue.clone());
    for (key, value) in definition.entries() {
        source.set(key, value.clone());
    }
    let source = Value::Rec(source);
    let mut resolver = Resolver { source: &source, cache: Vec::new() };
    resolver.resolve(&source, "theme", &[])
}

struct Resolver<'a> {
    source: &'a Value,
    cache: Vec<(String, Color)>,
}

impl Resolver<'_> {
    fn resolve(&mut self, value: &Value, path: &str, stack: &[String]) -> Result<Value, ThemeError> {
        match value {
            Value::Color(_) | Value::Num(_) => Ok(value.clone()),
            Value::Str(text) => self.resolve_color(text, path, stack).map(Value::Color),
            Value::Rec(rec) => {
                let mut out = Rec::new();
                for (key, item) in rec.entries() {
                    let resolved = self.resolve(item, &format!("{path}.{key}"), stack)?;
                    out.set(resolved_key(key), resolved);
                }
                Ok(Value::Rec(out))
            }
            _ => Err(ThemeError::new(format!("Invalid theme value at \"{path}\""))),
        }
    }

    fn resolve_color(&mut self, value: &str, path: &str, stack: &[String]) -> Result<Color, ThemeError> {
        if value == "transparent" {
            return Ok(Color::plain(RGBA::from_ints(0.0, 0.0, 0.0, 0.0)));
        }
        if is_hex(value) {
            return Ok(Color::plain(RGBA::from_hex(value)));
        }
        let Some(target) = value.strip_prefix('$') else {
            return Err(ThemeError::new(format!("Invalid color \"{value}\" at \"{path}\"")));
        };
        if let Some((_, hit)) = self.cache.iter().find(|(k, _)| k == target) {
            return Ok(*hit);
        }
        if stack.iter().any(|s| s == target) {
            let chain: Vec<&str> = stack.iter().map(String::as_str).chain([target]).collect();
            return Err(ThemeError::new(format!("Circular theme reference: {}", chain.join(" -> "))));
        }
        let found = read_color(self.source, target)?;
        let mut next = stack.to_vec();
        next.push(target.to_owned());
        let result = self.resolve(&found, target, &next)?;
        let Value::Color(color) = result else {
            return Err(ThemeError::new(format!("Theme reference \"{value}\" at \"{path}\" is not a color")));
        };
        self.cache.push((target.to_owned(), color));
        Ok(color)
    }
}

static HUE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^hue\.([^\n\r\u{2028}\u{2029}]+)\.(100|200|300|400|500|600|700|800|900)$").unwrap()
});

fn read_color(source: &Value, path: &str) -> Result<Value, ThemeError> {
    let Some(found) = HUE_PATH.captures(path) else { return read(source, path) };
    let name = &found[1];
    let step = &found[2];
    let color = source.at("hue").at(name).at(step);
    if color.is_undefined() {
        return Err(ThemeError::new(format!("Theme reference \"${path}\" was not found")));
    }
    Ok(color.clone())
}

fn resolved_key(key: &str) -> String {
    match key.strip_prefix('$') {
        Some(state) if ACTION_STATES.contains(&state) => state.to_owned(),
        _ => key.to_owned(),
    }
}

fn read(source: &Value, path: &str) -> Result<Value, ThemeError> {
    let mut value = source;
    for key in path.split('.') {
        value = match value.as_rec() {
            Some(rec) => match rec.get(key) {
                Some(found) => found,
                None if is_prototype_property(key) => {
                    return Err(ThemeError::new(format!("Invalid theme value at \"{path}\"")));
                }
                None => &Value::Undefined,
            },
            None => &Value::Undefined,
        };
    }
    if value.is_undefined() {
        return Err(ThemeError::new(format!("Theme reference \"${path}\" was not found")));
    }
    Ok(value.clone())
}

/// `resolveHue(definition.hue)`: every hue name to its nine colors; aliases (`$hue.<name>`) are copies of their
/// target's scale, with their own identity.
fn resolve_hue(definition: &Value, theme: u32) -> Result<HueTable, ThemeError> {
    let source = definition.as_rec().cloned().unwrap_or_default();
    let mut cache: Vec<(String, [RGBA; 9])> = Vec::new();
    fn resolve(source: &Rec, cache: &mut Vec<(String, [RGBA; 9])>, name: &str, stack: &[String]) -> Result<[RGBA; 9], ThemeError> {
        if let Some((_, hit)) = cache.iter().find(|(k, _)| k == name) {
            return Ok(*hit);
        }
        if stack.iter().any(|s| s == name) {
            let chain: Vec<&str> = stack.iter().map(String::as_str).chain([name]).collect();
            return Err(ThemeError::new(format!("Circular hue reference: {}", chain.join(" -> "))));
        }
        let value = source.get(name).unwrap_or(&Value::Undefined);
        if let Value::Str(text) = value {
            let target = text.strip_prefix("$hue.").filter(|t| !t.is_empty() && !t.contains(['\n', '\r', '\u{2028}', '\u{2029}']));
            let Some(target) = target else {
                return Err(ThemeError::new(format!("Hue alias \"{text}\" must reference a hue scale")));
            };
            if source.get(target).is_none_or(Value::is_undefined) && !is_prototype_property(target) {
                return Err(ThemeError::new(format!("Hue alias \"{text}\" references a missing hue")));
            }
            let mut next = stack.to_vec();
            next.push(name.to_owned());
            let scale = resolve(source, cache, target, &next)?;
            cache.push((name.to_owned(), scale));
            return Ok(scale);
        }
        let Some(rec) = value.as_rec() else {
            return Err(ThemeError::new(format!("Hue \"{name}\" was not found")));
        };
        let mut scale = [RGBA::default(); 9];
        for (i, step) in HUE_STEPS.iter().enumerate() {
            match rec.get(&step.to_string()) {
                Some(Value::Str(color)) if is_hex(color) => scale[i] = RGBA::from_hex(color),
                _ => return Err(ThemeError::new(format!("Invalid hue color at \"hue.{name}.{step}\""))),
            }
        }
        for step in rec.keys() {
            let number = js::to_number(step);
            let known = HUE_STEPS.iter().any(|s| *s as f64 == number);
            if !known {
                return Err(ThemeError::new(format!("Unknown hue step at \"hue.{name}.{step}\"")));
            }
        }
        cache.push((name.to_owned(), scale));
        Ok(scale)
    }
    let mut table = HueTable { names: Vec::new(), scales: Vec::new() };
    for (index, name) in source.keys().enumerate() {
        let colors = resolve(&source, &mut cache, name, &[])?;
        let mut scale = HueScale([Color::plain(RGBA::default()); 9]);
        for (position, rgba) in colors.into_iter().enumerate() {
            scale.0[position] =
                Color { rgba, origin: Some(Origin { theme, hue: index as u16, position: position as u8 }) };
        }
        table.names.push(name.to_owned());
        table.scales.push(scale);
    }
    Ok(table)
}
