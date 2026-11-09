//! `addons/universal/default-parser.ts`: the built-in binding syntax (`ctrl+x`, `<leader>a`, `gg`, `ctrl + shift + k`)
//! and the default event matcher.
//!
//! The parser walks the input in UTF-16 units, as TS indexes it: a character outside the BMP is two units, so
//! `"😀"` is a two-stroke sequence of its surrogates (each U+FFFD here, see the crate docs); no event matches either.
use super::keys::{KeyMatch, KeySequencePart, KeyStrokeInput, create_key_sequence_part, normalize_binding_token_name};
use std::collections::HashMap;

/// `ResolvedKeyToken`.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedToken {
    pub stroke: super::keys::KeyStroke,
    pub match_: KeyMatch,
}

/// `BindingParserContext` (no sequence patterns: see the module docs of `keymap`).
pub struct ParserContext<'a> {
    pub input: &'a [u16],
    pub index: usize,
    pub tokens: &'a HashMap<String, ResolvedToken>,
}

/// `BindingParserResult`.
#[derive(Debug, Default)]
pub struct ParserResult {
    pub parts: Vec<KeySequencePart>,
    pub next_index: usize,
    pub used_tokens: Vec<String>,
    pub unknown_tokens: Vec<String>,
}

const NAMED_SINGLE_STROKE_KEYS: &[&str] = &[
    "up",
    "down",
    "left",
    "right",
    "clear",
    "escape",
    "return",
    "linefeed",
    "enter",
    "tab",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "space",
    "lt",
    "gt",
    "plus",
    "minus",
    "equal",
    "comma",
    "period",
    "slash",
    "backslash",
    "semicolon",
    "quote",
    "backquote",
    "leftbracket",
    "rightbracket",
    "capslock",
    "numlock",
    "scrolllock",
    "printscreen",
    "pause",
    "menu",
    "apps",
    "kp0",
    "kp1",
    "kp2",
    "kp3",
    "kp4",
    "kp5",
    "kp6",
    "kp7",
    "kp8",
    "kp9",
    "kpdecimal",
    "kpdivide",
    "kpmultiply",
    "kpminus",
    "kpplus",
    "kpenter",
    "kpequal",
    "kpseparator",
    "kpleft",
    "kpright",
    "kpup",
    "kpdown",
    "kppageup",
    "kppagedown",
    "kphome",
    "kpend",
    "kpinsert",
    "kpdelete",
    "mediaplay",
    "mediapause",
    "mediaplaypause",
    "mediareverse",
    "mediastop",
    "mediafastforward",
    "mediarewind",
    "medianext",
    "mediaprev",
    "mediarecord",
    "volumedown",
    "volumeup",
    "mute",
    "leftshift",
    "leftctrl",
    "leftalt",
    "leftsuper",
    "lefthyper",
    "leftmeta",
    "rightshift",
    "rightctrl",
    "rightalt",
    "rightsuper",
    "righthyper",
    "rightmeta",
    "iso_level3_shift",
    "iso_level5_shift",
    "option",
    "alt",
    "meta",
    "super",
    "hyper",
    "control",
    "ctrl",
    "shift",
];
const MODIFIER_KEY_NAMES: &[&str] = &["ctrl", "control", "shift", "meta", "alt", "option", "super", "hyper"];

fn lower_ascii(unit: u16) -> u16 {
    if (65..=90).contains(&unit) { unit + 32 } else { unit }
}

fn is_digit(unit: Option<u16>) -> bool {
    unit.is_some_and(|unit| (48..=57).contains(&unit))
}

fn text(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `startsWithAsciiInsensitive`.
fn starts_with_ascii_insensitive(input: &[u16], prefix: &str, index: usize) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    if index + prefix.len() > input.len() {
        return false;
    }
    input[index..index + prefix.len()].iter().zip(&prefix).all(|(&unit, &expected)| lower_ascii(unit) == expected)
}

/// `findBucketedPrefixMatch`: the longest candidate starting at `index`, ASCII case-insensitively.
fn find_prefix_match(candidates: &[&'static str], input: &[u16], index: usize) -> Option<&'static str> {
    let first = lower_ascii(*input.get(index)?);
    candidates
        .iter()
        .filter(|candidate| candidate.encode_utf16().next() == Some(first))
        .filter(|candidate| starts_with_ascii_insensitive(input, candidate, index))
        .max_by_key(|candidate| candidate.len())
        .copied()
}

/// `findNamedSingleStrokeKey`.
fn find_named_single_stroke_key(input: &[u16], index: usize) -> Option<String> {
    if let Some(named) = find_prefix_match(NAMED_SINGLE_STROKE_KEYS, input, index) {
        return Some(named.to_owned());
    }
    if lower_ascii(*input.get(index)?) != 102 || !is_digit(input.get(index + 1).copied()) {
        return None;
    }
    let end = if is_digit(input.get(index + 2).copied()) { index + 3 } else { index + 2 };
    Some(text(&input[index..end]).to_lowercase())
}

fn modifier_display(ctrl: bool, shift: bool, meta: bool, super_: bool, hyper: bool, name: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if ctrl {
        parts.push("ctrl");
    }
    if shift {
        parts.push("shift");
    }
    if meta {
        parts.push("meta");
    }
    if super_ {
        parts.push("super");
    }
    if hyper {
        parts.push("hyper");
    }
    parts.push(name);
    parts.join("+")
}

fn stroke(name: String, ctrl: bool, shift: bool, meta: bool, super_: bool, hyper: bool) -> KeyStrokeInput {
    KeyStrokeInput {
        name,
        ctrl: Some(ctrl),
        shift: Some(shift),
        meta: Some(meta),
        super_: Some(super_),
        hyper: if hyper { Some(true) } else { None },
    }
}

/// `parseStringKeyPart`: `ctrl+shift+k`, split on `+`.
fn parse_string_key_part(input: &str) -> Result<KeySequencePart, String> {
    if input == " " {
        return create_key_sequence_part(&KeyStrokeInput::named("space"), Some("space".to_owned()), None, None);
    }
    if input == "+" {
        return create_key_sequence_part(&KeyStrokeInput::named("+"), Some("+".to_owned()), None, None);
    }
    let (mut ctrl, mut shift, mut meta, mut super_, mut hyper) = (false, false, false, false, false);
    let mut name = String::new();
    let mut display_name = String::new();
    for raw in input.split('+') {
        let part = super::keys::js_trim(raw);
        if part.is_empty() {
            continue;
        }
        let lowered = part.to_lowercase();
        match lowered.as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "meta" | "alt" | "option" => meta = true,
            "super" => super_ = true,
            "hyper" => hyper = true,
            _ => {
                if !name.is_empty() {
                    return Err(format!("Invalid key \"{input}\": multiple key names are not supported"));
                }
                name = part.to_owned();
                display_name = lowered;
            }
        }
    }
    if name.is_empty() {
        return Err(format!("Invalid key \"{input}\": missing key name"));
    }
    let display = modifier_display(ctrl, shift, meta, super_, hyper, &display_name);
    create_key_sequence_part(&stroke(name, ctrl, shift, meta, super_, hyper), Some(display), None, None)
}

/// `parseModifiedKeyPart`: modifiers each followed by `+`, then a named key or one unit.
fn parse_modified_key_part(input: &[u16], index: usize) -> Result<Option<(KeySequencePart, usize)>, String> {
    let mut cursor = index;
    let (mut ctrl, mut shift, mut meta, mut super_, mut hyper) = (false, false, false, false, false);
    while cursor < input.len() {
        let Some(modifier) = find_prefix_match(MODIFIER_KEY_NAMES, input, cursor) else { break };
        let plus = cursor + modifier.len();
        if input.get(plus) != Some(&(b'+' as u16)) {
            break;
        }
        match modifier {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "meta" | "alt" | "option" => meta = true,
            "super" => super_ = true,
            _ => hyper = true,
        }
        cursor = plus + 1;
    }
    if cursor == index {
        return Ok(None);
    }
    let Some(&unit) = input.get(cursor) else {
        return Err(format!("Invalid key \"{}\": missing key name", text(&input[index..])));
    };
    let name = find_named_single_stroke_key(input, cursor).unwrap_or_else(|| text(&[unit]));
    let length = name.encode_utf16().count().max(1);
    let display_name = match name.as_str() {
        " " => "space".to_owned(),
        "+" => "+".to_owned(),
        _ => name.to_lowercase(),
    };
    let display = modifier_display(ctrl, shift, meta, super_, hyper, &display_name);
    let name = if name == " " { "space".to_owned() } else { name };
    let part = create_key_sequence_part(&stroke(name, ctrl, shift, meta, super_, hyper), Some(display), None, None)?;
    Ok(Some((part, cursor + length)))
}

fn is_js_whitespace_unit(unit: u16) -> bool {
    char::from_u32(unit as u32).is_some_and(crate::js::is_js_space)
}

/// `defaultBindingParser`.
pub fn default_binding_parser(ctx: &ParserContext<'_>) -> Result<Option<ParserResult>, String> {
    let input = ctx.input;
    let index = ctx.index;
    if index == 0 && input.contains(&(b'+' as u16)) && input.iter().any(|&unit| is_js_whitespace_unit(unit)) {
        return Ok(Some(ParserResult {
            parts: vec![parse_string_key_part(&text(input))?],
            next_index: input.len(),
            ..ParserResult::default()
        }));
    }
    let Some(&unit) = input.get(index) else { return Ok(None) };
    let single = |next_index| -> Result<Option<ParserResult>, String> {
        Ok(Some(ParserResult {
            parts: vec![parse_string_key_part(&text(&[unit]))?],
            next_index,
            ..ParserResult::default()
        }))
    };
    if unit == b'<' as u16 || unit == b'{' as u16 {
        let close = if unit == b'<' as u16 { b'>' } else { b'}' } as u16;
        let Some(end) = input[index..].iter().position(|&candidate| candidate == close).map(|offset| index + offset)
        else {
            return single(index + 1);
        };
        let name = normalize_binding_token_name(&text(&input[index + 1..end]))?;
        // No sequence patterns are registered, so a `{name}` is always unknown.
        let token = if unit == b'<' as u16 { ctx.tokens.get(&name) } else { None };
        let Some(token) = token else {
            return Ok(Some(ParserResult {
                parts: Vec::new(),
                next_index: end + 1,
                used_tokens: Vec::new(),
                unknown_tokens: vec![name],
            }));
        };
        let input = KeyStrokeInput::from(&token.stroke);
        let part =
            create_key_sequence_part(&input, Some(format!("<{name}>")), Some(token.match_.clone()), Some(&name))?;
        return Ok(Some(ParserResult {
            parts: vec![part],
            next_index: end + 1,
            used_tokens: vec![name],
            unknown_tokens: Vec::new(),
        }));
    }
    if let Some((part, next_index)) = parse_modified_key_part(input, index)? {
        return Ok(Some(ParserResult { parts: vec![part], next_index, ..ParserResult::default() }));
    }
    if let Some(named) = find_named_single_stroke_key(input, index) {
        let length = named.encode_utf16().count();
        let normalized = super::keys::js_trim(&named).to_lowercase();
        let part = create_key_sequence_part(&KeyStrokeInput::named(normalized.clone()), Some(normalized), None, None)?;
        return Ok(Some(ParserResult { parts: vec![part], next_index: index + length, ..ParserResult::default() }));
    }
    single(index + 1)
}
