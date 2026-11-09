//! `services/keys.ts`: key strokes, sequence parts, match ids and their canonical strings.
use std::rc::Rc;

/// A match id: `key:<name>:<ctrl>:<shift>:<meta>:<super>:<hyper>` or `text:<id>`.
pub type KeyMatch = Rc<str>;

/// `NormalizedKeyStroke` (`hyper` false is TS `undefined`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyStroke {
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_: bool,
    pub hyper: bool,
}

/// `KeyStrokeInput`: a stroke whose modifiers default to false.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyStrokeInput {
    pub name: String,
    pub ctrl: Option<bool>,
    pub shift: Option<bool>,
    pub meta: Option<bool>,
    pub super_: Option<bool>,
    pub hyper: Option<bool>,
}

impl KeyStrokeInput {
    pub fn named(name: impl Into<String>) -> KeyStrokeInput {
        KeyStrokeInput { name: name.into(), ..KeyStrokeInput::default() }
    }
}

impl From<&KeyStroke> for KeyStrokeInput {
    fn from(stroke: &KeyStroke) -> Self {
        KeyStrokeInput {
            name: stroke.name.clone(),
            ctrl: Some(stroke.ctrl),
            shift: Some(stroke.shift),
            meta: Some(stroke.meta),
            super_: Some(stroke.super_),
            hyper: Some(stroke.hyper),
        }
    }
}

/// `KeyLike`: a binding string or a stroke.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyLike {
    Str(String),
    Stroke(KeyStrokeInput),
}

impl From<&str> for KeyLike {
    fn from(text: &str) -> Self {
        KeyLike::Str(text.to_owned())
    }
}

impl From<String> for KeyLike {
    fn from(text: String) -> Self {
        KeyLike::Str(text)
    }
}

impl From<KeyStrokeInput> for KeyLike {
    fn from(stroke: KeyStrokeInput) -> Self {
        KeyLike::Stroke(stroke)
    }
}

/// `KeySequencePart`.
#[derive(Clone, Debug, PartialEq)]
pub struct KeySequencePart {
    pub stroke: KeyStroke,
    pub display: String,
    pub match_: KeyMatch,
    pub token_name: Option<String>,
}

/// `String.prototype.trim` (ECMAScript white space and line terminators).
pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(crate::js::is_js_space)
}

/// `normalizeBindingTokenName`.
pub fn normalize_binding_token_name(token: &str) -> Result<String, String> {
    let normalized = js_trim(token).to_lowercase();
    if normalized.is_empty() {
        return Err("Invalid keymap token: token cannot be empty".to_owned());
    }
    Ok(normalized)
}

/// `normalizeKeyName`.
pub fn normalize_key_name(name: &str) -> Result<String, String> {
    let normalized = js_trim(name).to_lowercase();
    if normalized.is_empty() {
        return Err("Invalid key name: key name cannot be empty".to_owned());
    }
    Ok(normalized)
}

/// `normalizeKeyStroke`.
pub fn normalize_key_stroke(input: &KeyStrokeInput) -> Result<KeyStroke, String> {
    Ok(KeyStroke {
        name: normalize_key_name(&input.name)?,
        ctrl: input.ctrl.unwrap_or(false),
        shift: input.shift.unwrap_or(false),
        meta: input.meta.unwrap_or(false),
        super_: input.super_.unwrap_or(false),
        hyper: input.hyper.unwrap_or(false),
    })
}

/// `createKeySequencePart`.
pub fn create_key_sequence_part(
    input: &KeyStrokeInput,
    display: Option<String>,
    match_: Option<KeyMatch>,
    token_name: Option<&str>,
) -> Result<KeySequencePart, String> {
    let stroke = normalize_key_stroke(input)?;
    let token_name = match token_name {
        Some(token) if !token.is_empty() => Some(normalize_binding_token_name(token)?),
        _ => None,
    };
    Ok(KeySequencePart {
        display: display.unwrap_or_else(|| stringify_canonical_stroke(&stroke)),
        match_: match_.unwrap_or_else(|| create_key_match_for(&stroke)),
        stroke,
        token_name,
    })
}

/// `createKeyMatch`.
pub fn create_key_match(input: &KeyStrokeInput) -> Result<KeyMatch, String> {
    Ok(create_key_match_for(&normalize_key_stroke(input)?))
}

pub(crate) fn create_key_match_for(stroke: &KeyStroke) -> KeyMatch {
    let bit = |value: bool| if value { 1 } else { 0 };
    format!(
        "key:{}:{}:{}:{}:{}:{}",
        stroke.name,
        bit(stroke.ctrl),
        bit(stroke.shift),
        bit(stroke.meta),
        bit(stroke.super_),
        bit(stroke.hyper)
    )
    .into()
}

/// `createTextKeyMatch`.
pub fn create_text_key_match(id: &str) -> Result<KeyMatch, String> {
    let normalized = js_trim(id);
    if normalized.is_empty() {
        return Err("Invalid keymap match id: id cannot be empty".to_owned());
    }
    Ok(format!("text:{normalized}").into())
}

/// `stringifyKeyStroke` of a stroke.
pub fn stringify_key_stroke(stroke: &KeyStroke) -> String {
    stringify_canonical_stroke(stroke)
}

/// `stringifyKeyStroke` of a sequence part (`preferDisplay` takes its display).
pub fn stringify_key_part(part: &KeySequencePart, prefer_display: bool) -> String {
    if prefer_display && !part.display.is_empty() {
        return part.display.clone();
    }
    stringify_canonical_stroke(&part.stroke)
}

/// `stringifyKeySequence`.
pub fn stringify_key_sequence(parts: &[KeySequencePart], prefer_display: bool, separator: &str) -> String {
    parts.iter().map(|part| stringify_key_part(part, prefer_display)).collect::<Vec<_>>().join(separator)
}

fn stringify_canonical_stroke(stroke: &KeyStroke) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if stroke.ctrl {
        parts.push("ctrl");
    }
    if stroke.shift {
        parts.push("shift");
    }
    if stroke.meta {
        parts.push("meta");
    }
    if stroke.super_ {
        parts.push("super");
    }
    if stroke.hyper {
        parts.push("hyper");
    }
    parts.push(if stroke.name == "return" { "enter" } else { &stroke.name });
    parts.join("+")
}
