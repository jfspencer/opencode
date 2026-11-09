//! `packages/core/src/lib/parse.keypress-kitty.ts`: the Kitty keyboard protocol (`CSI code:shifted:base ;
//! mods:event ; text u` and the functional/tilde keys with an event type).
use crate::js::{from_code_point, parse_int, to_int32, to_upper, utf16_len};
use crate::keypress::{KeyEventType, KeySource, ParsedKey};

/// `kittyKeyMap`.
pub fn kitty_key_name(code: f64) -> Option<&'static str> {
    if code.fract() != 0.0 || !(0.0..=57454.0).contains(&code) {
        return None;
    }
    // A negative zero names property "0", which the map lacks.
    Some(match code as u32 {
        27 => "escape",
        9 => "tab",
        13 => "return",
        127 => "backspace",
        57344 => "escape",
        57345 => "return",
        57346 => "tab",
        57347 => "backspace",
        57348 => "insert",
        57349 => "delete",
        57350 => "left",
        57351 => "right",
        57352 => "up",
        57353 => "down",
        57354 => "pageup",
        57355 => "pagedown",
        57356 => "home",
        57357 => "end",
        57358 => "capslock",
        57359 => "scrolllock",
        57360 => "numlock",
        57361 => "printscreen",
        57362 => "pause",
        57363 => "menu",
        code @ 57364..=57398 => FUNCTION_KEYS[(code - 57364) as usize],
        code @ 57399..=57408 => KEYPAD_DIGITS[(code - 57399) as usize],
        57409 => "kpdecimal",
        57410 => "kpdivide",
        57411 => "kpmultiply",
        57412 => "kpminus",
        57413 => "kpplus",
        57414 => "kpenter",
        57415 => "kpequal",
        57416 => "kpseparator",
        57417 => "kpleft",
        57418 => "kpright",
        57419 => "kpup",
        57420 => "kpdown",
        57421 => "kppageup",
        57422 => "kppagedown",
        57423 => "kphome",
        57424 => "kpend",
        57425 => "kpinsert",
        57426 => "kpdelete",
        57427 => "clear",
        57428 => "mediaplay",
        57429 => "mediapause",
        57430 => "mediaplaypause",
        57431 => "mediareverse",
        57432 => "mediastop",
        57433 => "mediafastforward",
        57434 => "mediarewind",
        57435 => "medianext",
        57436 => "mediaprev",
        57437 => "mediarecord",
        57438 => "volumedown",
        57439 => "volumeup",
        57440 => "mute",
        57441 => "leftshift",
        57442 => "leftctrl",
        57443 => "leftalt",
        57444 => "leftsuper",
        57445 => "lefthyper",
        57446 => "leftmeta",
        57447 => "rightshift",
        57448 => "rightctrl",
        57449 => "rightalt",
        57450 => "rightsuper",
        57451 => "righthyper",
        57452 => "rightmeta",
        57453 => "iso_level3_shift",
        57454 => "iso_level5_shift",
        _ => return None,
    })
}

const FUNCTION_KEYS: [&str; 35] = [
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13", "f14", "f15", "f16", "f17",
    "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31", "f32", "f33",
    "f34", "f35",
];
const KEYPAD_DIGITS: [&str; 10] = ["kp0", "kp1", "kp2", "kp3", "kp4", "kp5", "kp6", "kp7", "kp8", "kp9"];

/// The codes of `kittyKeyMap`, in its insertion order (JS orders integer keys ascending).
fn kitty_codes() -> impl Iterator<Item = u32> {
    [9u32, 13, 27, 127].into_iter().chain(57344..=57454)
}

/// `kittyNamedSingleStrokeKeys`: the distinct names of `kittyKeyMap`, first occurrence first.
pub fn kitty_named_single_stroke_keys() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for code in kitty_codes() {
        let name = kitty_key_name(code as f64).unwrap();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// `printableKeypadText`.
pub fn printable_keypad_text(name: &str) -> Option<&'static str> {
    Some(match name {
        "kp0" => "0",
        "kp1" => "1",
        "kp2" => "2",
        "kp3" => "3",
        "kp4" => "4",
        "kp5" => "5",
        "kp6" => "6",
        "kp7" => "7",
        "kp8" => "8",
        "kp9" => "9",
        "kpdecimal" => ".",
        "kpdivide" => "/",
        "kpmultiply" => "*",
        "kpminus" => "-",
        "kpplus" => "+",
        "kpequal" => "=",
        "kpseparator" => ",",
        _ => return None,
    })
}

fn functional_key(letter: char) -> Option<&'static str> {
    Some(match letter {
        'A' => "up",
        'B' => "down",
        'C' => "right",
        'D' => "left",
        'H' => "home",
        'F' => "end",
        'E' => "clear",
        'P' => "f1",
        'Q' => "f2",
        'S' => "f4",
        _ => return None,
    })
}

fn tilde_key(number: &str) -> Option<&'static str> {
    Some(match number {
        "1" => "home",
        "2" => "insert",
        "3" => "delete",
        "4" => "end",
        "5" => "pageup",
        "6" => "pagedown",
        "7" => "home",
        "8" => "end",
        "11" => "f1",
        "12" => "f2",
        "13" => "f3",
        "14" => "f4",
        "15" => "f5",
        "17" => "f6",
        "18" => "f7",
        "19" => "f8",
        "20" => "f9",
        "21" => "f10",
        "23" => "f11",
        "24" => "f12",
        "29" => "menu",
        "57427" => "clear",
        _ => return None,
    })
}

fn kitty_key(sequence: &str) -> ParsedKey {
    ParsedKey {
        name: String::new(),
        ctrl: false,
        meta: false,
        shift: false,
        option: false,
        sequence: sequence.to_owned(),
        number: false,
        raw: sequence.to_owned(),
        event_type: KeyEventType::Press,
        source: KeySource::Kitty,
        code: None,
        super_: Some(false),
        hyper: Some(false),
        caps_lock: Some(false),
        num_lock: Some(false),
        base_code: None,
        repeated: None,
    }
}

/// The modifier field: a mask above 1 sets the modifiers of `mask - 1` (`fromKittyMods`).
fn apply_modifiers(key: &mut ParsedKey, field: &str) {
    if field.is_empty() {
        return;
    }
    let mask = parse_int(field);
    if mask.is_nan() || mask <= 1.0 {
        return;
    }
    let mods = to_int32(mask - 1.0);
    let alt = mods & 2 != 0;
    key.shift = mods & 1 != 0;
    key.ctrl = mods & 4 != 0;
    key.meta = alt || mods & 32 != 0;
    key.option = alt;
    key.super_ = Some(mods & 8 != 0);
    key.hyper = Some(mods & 16 != 0);
    key.caps_lock = Some(mods & 64 != 0);
    key.num_lock = Some(mods & 128 != 0);
}

fn apply_event_type(key: &mut ParsedKey, event_type: Option<&str>) {
    match event_type {
        None | Some("") | Some("1") => key.event_type = KeyEventType::Press,
        Some("2") => {
            key.event_type = KeyEventType::Press;
            key.repeated = Some(true);
        }
        Some("3") => key.event_type = KeyEventType::Release,
        Some(_) => key.event_type = KeyEventType::Press,
    }
}

/// `parseKittySpecialKey`: `^\x1b\[(\d+);(\d+):(\d+)([A-Z~])$`.
fn parse_special_key(sequence: &str) -> Option<ParsedKey> {
    let body = sequence.strip_prefix("\x1b[")?;
    let digits = |text: &str| text.bytes().take_while(u8::is_ascii_digit).count();
    let first = digits(body);
    if first == 0 || body.as_bytes().get(first) != Some(&b';') {
        return None;
    }
    let rest = &body[first + 1..];
    let second = digits(rest);
    if second == 0 || rest.as_bytes().get(second) != Some(&b':') {
        return None;
    }
    let tail = &rest[second + 1..];
    let third = digits(tail);
    if third == 0 || tail.len() != third + 1 {
        return None;
    }
    let terminator = tail.as_bytes()[third] as char;
    if !(terminator == '~' || terminator.is_ascii_uppercase()) {
        return None;
    }
    let number = &body[..first];
    let name = if terminator == '~' {
        tilde_key(number)?
    } else {
        if number != "1" {
            return None;
        }
        functional_key(terminator)?
    };
    let mut key = kitty_key(sequence);
    key.name = name.to_owned();
    apply_modifiers(&mut key, &rest[..second]);
    // An event type other than 1, 2 or 3 leaves the press.
    match &tail[..third] {
        "2" => {
            key.event_type = KeyEventType::Press;
            key.repeated = Some(true);
        }
        "3" => key.event_type = KeyEventType::Release,
        _ => key.event_type = KeyEventType::Press,
    }
    Some(key)
}

fn is_valid_code_point(value: f64) -> bool {
    !value.is_nan() && value > 0.0 && value <= 1114111.0
}

/// `parseKittyKeyboard`.
pub fn parse_kitty_keyboard(sequence: &str) -> Option<ParsedKey> {
    if let Some(key) = parse_special_key(sequence) {
        return Some(key);
    }
    // ^\x1b\[([^\x1b]+)u$
    let params = sequence.strip_prefix("\x1b[")?.strip_suffix('u')?;
    if params.is_empty() || params.contains('\x1b') {
        return None;
    }
    let fields: Vec<&str> = params.split(';').collect();
    let mut key = kitty_key(sequence);
    let mut text = String::new();

    let field1: Vec<&str> = fields[0].split(':').collect();
    let code_text = field1[0];
    if code_text.is_empty() {
        return None;
    }
    let code = parse_int(code_text);
    if code.is_nan() {
        return None;
    }
    let mut shifted = None;
    let mut base = None;
    if let Some(value) = field1.get(1).filter(|value| !value.is_empty()) {
        let value = parse_int(value);
        if is_valid_code_point(value) {
            shifted = Some(value as u32);
        }
    }
    if let Some(value) = field1.get(2).filter(|value| !value.is_empty()) {
        let value = parse_int(value);
        if is_valid_code_point(value) {
            base = Some(value as u32);
        }
    }

    let known = kitty_key_name(code);
    if let Some(name) = known {
        key.name = name.to_owned();
        key.code = Some(format!("[{}u", code as u32));
    } else if code == 0.0 {
        key.name = String::new();
    } else if is_valid_code_point(code) {
        let ch = from_code_point(code as u32);
        key.name = if ch == ' ' { "space".to_owned() } else { ch.to_string() };
        if let Some(base) = base {
            key.base_code = Some(base);
        }
    } else {
        return None;
    }

    if let Some(field2) = fields.get(1).filter(|field| !field.is_empty()) {
        let mut parts = field2.split(':');
        apply_modifiers(&mut key, parts.next().unwrap_or(""));
        apply_event_type(&mut key, parts.next());
    }

    if let Some(field3) = fields.get(2).filter(|field| !field.is_empty()) {
        for part in field3.split(':') {
            let value = parse_int(part);
            if is_valid_code_point(value) {
                text.push(from_code_point(value as u32));
            }
        }
    }

    if text.is_empty() {
        text = printable_keypad_text(&key.name).unwrap_or("").to_owned();
    }

    if text.is_empty() && !key.name.is_empty() && known.is_none() {
        if code == 32.0 {
            text = " ".to_owned();
        } else if key.shift
            && let Some(shifted) = shifted
        {
            text = from_code_point(shifted).to_string();
        } else if key.shift && utf16_len(&key.name) == 1 {
            text = to_upper(&key.name);
        } else {
            text = key.name.clone();
        }
    }

    if !text.is_empty() {
        if code == 0.0 {
            key.name = text.clone();
        }
        key.sequence = text.clone();
    }

    if code == 0.0 && text.is_empty() {
        return None;
    }
    Some(key)
}
