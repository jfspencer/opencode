//! `packages/core/src/lib/parse.keypress.ts`: one key's bytes (as the stdin parser frames them) to a `ParsedKey`,
//! or `None` for what is not a key (mouse reports, terminal replies, focus, paste markers). The regular expressions
//! of the TS source are hand-written matchers named after them.
use crate::js::{from_char_code, parse_int, to_int32, utf16_len};
use crate::kitty::{kitty_named_single_stroke_keys, parse_kitty_keyboard};

/// `KeyEventType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

impl KeyEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyEventType::Press => "press",
            KeyEventType::Repeat => "repeat",
            KeyEventType::Release => "release",
        }
    }
}

/// `ParsedKey.source`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeySource {
    Raw,
    Kitty,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Raw => "raw",
            KeySource::Kitty => "kitty",
        }
    }
}

/// `ParsedKey`. The optional TS fields are `Option`s: `None` is `undefined`. `super_` is TS `super`.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedKey {
    pub name: String,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    pub option: bool,
    pub sequence: String,
    pub number: bool,
    pub raw: String,
    pub event_type: KeyEventType,
    pub source: KeySource,
    pub code: Option<String>,
    pub super_: Option<bool>,
    pub hyper: Option<bool>,
    pub caps_lock: Option<bool>,
    pub num_lock: Option<bool>,
    /// Kitty's base-layout code point (99 is `c`).
    pub base_code: Option<u32>,
    pub repeated: Option<bool>,
}

/// `parseKeypress`'s `keyName` table.
fn key_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "OP" => "f1",
        "OQ" => "f2",
        "OR" => "f3",
        "OS" => "f4",
        "[11~" => "f1",
        "[12~" => "f2",
        "[13~" => "f3",
        "[14~" => "f4",
        "[[A" => "f1",
        "[[B" => "f2",
        "[[C" => "f3",
        "[[D" => "f4",
        "[[E" => "f5",
        "[15~" => "f5",
        "[17~" => "f6",
        "[18~" => "f7",
        "[19~" => "f8",
        "[20~" => "f9",
        "[21~" => "f10",
        "[23~" => "f11",
        "[24~" => "f12",
        "[29~" => "menu",
        "[57427~" => "clear",
        "[A" => "up",
        "[B" => "down",
        "[C" => "right",
        "[D" => "left",
        "[E" => "clear",
        "[F" => "end",
        "[H" => "home",
        "[P" => "f1",
        "[Q" => "f2",
        "[S" => "f4",
        "OA" => "up",
        "OB" => "down",
        "OC" => "right",
        "OD" => "left",
        "OE" => "clear",
        "OF" => "end",
        "OH" => "home",
        "OM" => "return",
        "Oj" => "*",
        "Ok" => "+",
        "Ol" => ",",
        "Om" => "-",
        "On" => ".",
        "Oo" => "/",
        "Op" => "0",
        "Oq" => "1",
        "Or" => "2",
        "Os" => "3",
        "Ot" => "4",
        "Ou" => "5",
        "Ov" => "6",
        "Ow" => "7",
        "Ox" => "8",
        "Oy" => "9",
        "OX" => "=",
        "[1~" => "home",
        "[2~" => "insert",
        "[3~" => "delete",
        "[4~" => "end",
        "[5~" => "pageup",
        "[6~" => "pagedown",
        "[[5~" => "pageup",
        "[[6~" => "pagedown",
        "[7~" => "home",
        "[8~" => "end",
        "[a" => "up",
        "[b" => "down",
        "[c" => "right",
        "[d" => "left",
        "[e" => "clear",
        "f" => "right",
        "b" => "left",
        "p" => "up",
        "n" => "down",
        "[2$" => "insert",
        "[3$" => "delete",
        "[5$" => "pageup",
        "[6$" => "pagedown",
        "[7$" => "home",
        "[8$" => "end",
        "Oa" => "up",
        "Ob" => "down",
        "Oc" => "right",
        "Od" => "left",
        "Oe" => "clear",
        "[2^" => "insert",
        "[3^" => "delete",
        "[5^" => "pageup",
        "[6^" => "pagedown",
        "[7^" => "home",
        "[8^" => "end",
        "[Z" => "tab",
        _ => return None,
    })
}

/// The values of `keyName` in its order, as `Object.values` lists them.
const KEY_NAME_VALUES: [&str; 96] = [
    "f1", "f2", "f3", "f4", "f1", "f2", "f3", "f4", "f1", "f2", "f3", "f4", "f5", "f5", "f6", "f7", "f8", "f9", "f10",
    "f11", "f12", "menu", "clear", "up", "down", "right", "left", "clear", "end", "home", "f1", "f2", "f4", "up",
    "down", "right", "left", "clear", "end", "home", "return", "*", "+", ",", "-", ".", "/", "0", "1", "2", "3", "4",
    "5", "6", "7", "8", "9", "=", "home", "insert", "delete", "end", "pageup", "pagedown", "pageup", "pagedown",
    "home", "end", "up", "down", "right", "left", "clear", "right", "left", "up", "down", "insert", "delete", "pageup",
    "pagedown", "home", "end", "up", "down", "right", "left", "clear", "insert", "delete", "pageup", "pagedown",
    "home", "end", "tab", "",
];

/// `nonAlphanumericKeys`: `keyName`'s values and `backspace`.
pub fn non_alphanumeric_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = KEY_NAME_VALUES[..95].to_vec();
    keys.push("backspace");
    keys
}

/// `terminalNamedSingleStrokeKeys`.
pub fn terminal_named_single_stroke_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = Vec::new();
    let all = ["return", "linefeed", "tab", "escape", "space"]
        .into_iter()
        .chain(non_alphanumeric_keys())
        .chain(kitty_named_single_stroke_keys());
    for key in all {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

fn is_shift_key(code: &str) -> bool {
    matches!(code, "[a" | "[b" | "[c" | "[d" | "[e" | "[2$" | "[3$" | "[5$" | "[6$" | "[7$" | "[8$" | "[Z")
}

fn is_ctrl_key(code: &str) -> bool {
    matches!(code, "Oa" | "Ob" | "Oc" | "Od" | "Oe" | "[2^" | "[3^" | "[5^" | "[6^" | "[7^" | "[8^")
}

/// `getCtrlKeyName`.
fn ctrl_key_name(unit: u32) -> Option<String> {
    match unit {
        0 => Some("space".to_owned()),
        1..=26 => Some(char::from_u32(unit + 96).unwrap().to_string()),
        28..=31 => Some(char::from_u32(unit + 64).unwrap().to_string()),
        _ => None,
    }
}

/// `ss3NumpadPrintable`.
fn ss3_numpad_printable(code: &str) -> Option<&'static str> {
    Some(match code {
        "Op" => "0",
        "Oq" => "1",
        "Or" => "2",
        "Os" => "3",
        "Ot" => "4",
        "Ou" => "5",
        "Ov" => "6",
        "Ow" => "7",
        "Ox" => "8",
        "Oy" => "9",
        "Oj" => "*",
        "Ok" => "+",
        "Ol" => ",",
        "Om" => "-",
        "On" => ".",
        "Oo" => "/",
        "OX" => "=",
        _ => return None,
    })
}

fn digits(text: &str) -> usize {
    text.bytes().take_while(u8::is_ascii_digit).count()
}

/// `\d+` then each separator and `\d+` in turn; the rest of the string after the last group.
fn digit_groups<'a>(mut text: &'a str, separators: &[u8]) -> Option<&'a str> {
    let count = digits(text);
    if count == 0 {
        return None;
    }
    text = &text[count..];
    for &separator in separators {
        text = text.strip_prefix(separator as char)?;
        let count = digits(text);
        if count == 0 {
            return None;
        }
        text = &text[count..];
    }
    Some(text)
}

/// `[\d;]*` covering the whole text (`plus`: at least one).
fn digits_and_semicolons(text: &str, plus: bool) -> bool {
    (!plus || !text.is_empty()) && text.bytes().all(|byte| byte.is_ascii_digit() || byte == b';')
}

/// The filters before the key cases: SGR and X10 mouse reports, window and cursor reports, DA and DECRQM replies,
/// focus, OSC replies and paste markers are not keys.
fn is_filtered(s: &str) -> bool {
    // /^\x1b\[<\d+;\d+;\d+[Mm]$/ and the same without ESC
    for prefix in ["\x1b[<", "[<"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            if matches!(digit_groups(rest, b";;"), Some("M" | "m")) {
                return true;
            }
            // /^\x1b\[<[\d;]*$/ and /^\[<[\d;]*$/
            if digits_and_semicolons(rest, false) {
                return true;
            }
        }
    }
    if s.starts_with("\x1b[M") && utf16_len(s) >= 6 {
        return true;
    }
    if let Some(rest) = s.strip_prefix("\x1b[") {
        // /^\x1b\[\d+;\d+;\d+t$/
        if digit_groups(rest, b";;") == Some("t") {
            return true;
        }
        // /^\x1b\[\d+;\d+R$/
        if digit_groups(rest, b";") == Some("R") {
            return true;
        }
        if let Some(reply) = rest.strip_prefix('?') {
            // /^\x1b\[\?[\d;]+c$/ and /^\x1b\[\?[\d;]+\$y$/
            if let Some(body) = reply.strip_suffix('c')
                && digits_and_semicolons(body, true)
            {
                return true;
            }
            if let Some(body) = reply.strip_suffix("$y")
                && digits_and_semicolons(body, true)
            {
                return true;
            }
        }
    }
    if s == "\x1b[I" || s == "\x1b[O" {
        return true;
    }
    // /^\x1b\][\d;].*(\x1b\\|\x07)$/: `.` stops at line terminators.
    if let Some(rest) = s.strip_prefix("\x1b]") {
        let mut chars = rest.chars();
        if let Some(first) = chars.next()
            && (first.is_ascii_digit() || first == ';')
        {
            let body = chars.as_str();
            let rest = body.strip_suffix("\x1b\\").or_else(|| body.strip_suffix('\x07'));
            if let Some(middle) = rest
                && !middle.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            {
                return true;
            }
        }
    }
    s == "\x1b[200~" || s == "\x1b[201~"
}

/// What `fnKeyRe` captures: the introducer, the `\d+` before `[~^$]` and its `;\d+` modifier, the terminator,
/// the `\d+` before a letter, and the letter.
struct FnKey<'a> {
    introducer: &'a str,
    number: Option<&'a str>,
    modifier: Option<&'a str>,
    terminator: Option<&'a str>,
    letter_number: Option<&'a str>,
    letter: Option<&'a str>,
}

/// `/^(?:\x1b+)(O|N|\[|\[\[)(?:(\d+)(?:;(\d+))?([~^$])|(?:1;)?(\d+)?([a-zA-Z]))/`, a prefix match: the
/// introducers in order, the first that lets the rest match.
fn match_fn_key(s: &str) -> Option<FnKey<'_>> {
    let rest = s.trim_start_matches('\x1b');
    if rest.len() == s.len() {
        return None;
    }
    for introducer in ["O", "N", "[", "[["] {
        let Some(body) = rest.strip_prefix(introducer) else { continue };
        // (\d+)(?:;(\d+))?([~^$])
        let count = digits(body);
        if count > 0 {
            let number = &body[..count];
            let after = &body[count..];
            if let Some(next) = after.strip_prefix(';') {
                let modifier_count = digits(next);
                if modifier_count > 0 {
                    let tail = &next[modifier_count..];
                    if let Some(end) = tail.chars().next().filter(|c| matches!(c, '~' | '^' | '$')) {
                        return Some(FnKey {
                            introducer,
                            number: Some(number),
                            modifier: Some(&next[..modifier_count]),
                            terminator: Some(&tail[..end.len_utf8()]),
                            letter_number: None,
                            letter: None,
                        });
                    }
                }
            }
            if let Some(end) = after.chars().next().filter(|c| matches!(c, '~' | '^' | '$')) {
                return Some(FnKey {
                    introducer,
                    number: Some(number),
                    modifier: None,
                    terminator: Some(&after[..end.len_utf8()]),
                    letter_number: None,
                    letter: None,
                });
            }
        }
        // (?:1;)?(\d+)?([a-zA-Z])
        let candidates = match body.strip_prefix("1;") {
            Some(without) => [Some(without), Some(body)],
            None => [Some(body), None],
        };
        for candidate in candidates.into_iter().flatten() {
            let count = digits(candidate);
            let letter = &candidate[count..];
            if letter.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
                return Some(FnKey {
                    introducer,
                    number: None,
                    modifier: None,
                    terminator: None,
                    letter_number: (count > 0).then(|| &candidate[..count]),
                    letter: Some(&letter[..1]),
                });
            }
        }
    }
    None
}

fn new_key(s: &str) -> ParsedKey {
    ParsedKey {
        name: String::new(),
        ctrl: false,
        meta: false,
        shift: false,
        option: false,
        sequence: s.to_owned(),
        number: false,
        raw: s.to_owned(),
        event_type: KeyEventType::Press,
        source: KeySource::Raw,
        code: None,
        super_: None,
        hyper: None,
        caps_lock: None,
        num_lock: None,
        base_code: None,
        repeated: None,
    }
}

fn set_modifier_bits(key: &mut ParsedKey, modifier: i32) {
    key.ctrl = key.ctrl || modifier & 4 != 0;
    key.meta = key.meta || modifier & 2 != 0;
    key.shift = key.shift || modifier & 1 != 0;
    key.option = key.option || modifier & 2 != 0;
    key.super_ = Some(modifier & 8 != 0);
    key.hyper = Some(modifier & 16 != 0);
}

/// `parseKeypress(s, { useKittyKeyboard })` of a string.
pub fn parse_keypress(s: &str, use_kitty_keyboard: bool) -> Option<ParsedKey> {
    if is_filtered(s) {
        return None;
    }
    let mut key = new_key(s);
    let length = utf16_len(s);
    let first_unit = s.encode_utf16().next();
    let ctrl_name = if length == 1 { ctrl_key_name(first_unit.unwrap() as u32) } else { None };
    let meta_ctrl_name = if length == 2 && s.starts_with('\x1b') {
        ctrl_key_name(s.encode_utf16().nth(1).unwrap() as u32)
    } else {
        None
    };

    if use_kitty_keyboard && let Some(kitty) = parse_kitty_keyboard(s) {
        return Some(kitty);
    }

    // /^\x1b\[27;(\d+);(\d+)~$/
    if let Some(rest) = s.strip_prefix("\x1b[27;")
        && digit_groups(rest, b";") == Some("~")
    {
        let mut numbers = rest[..rest.len() - 1].split(';');
        let modifier = to_int32(parse_int(numbers.next().unwrap()) - 1.0);
        let char_code = parse_int(numbers.next().unwrap());
        key.ctrl = modifier & 4 != 0;
        key.meta = modifier & 2 != 0;
        key.shift = modifier & 1 != 0;
        key.option = modifier & 2 != 0;
        key.super_ = Some(modifier & 8 != 0);
        key.hyper = Some(modifier & 16 != 0);
        if char_code == 13.0 {
            key.name = "return".to_owned();
        } else if char_code == 27.0 {
            key.name = "escape".to_owned();
        } else if char_code == 9.0 {
            key.name = "tab".to_owned();
        } else if char_code == 32.0 {
            key.name = "space".to_owned();
        } else if char_code == 127.0 || char_code == 8.0 {
            key.name = "backspace".to_owned();
        } else {
            let ch = from_char_code(char_code).to_string();
            key.name = ch.clone();
            key.sequence = ch;
            if (48.0..=57.0).contains(&char_code) {
                key.number = true;
            }
        }
        return Some(key);
    }

    if s == "\r" || s == "\x1b\r" {
        key.name = "return".to_owned();
        key.meta = length == 2;
    } else if s == "\n" || s == "\x1b\n" {
        key.name = "linefeed".to_owned();
        key.meta = length == 2;
    } else if s == "\t" {
        key.name = "tab".to_owned();
    } else if s == "\x08" || s == "\x1b\x08" || s == "\x7f" || s == "\x1b\x7f" {
        key.name = "backspace".to_owned();
        key.meta = s.starts_with('\x1b');
    } else if s == "\x1b" || s == "\x1b\x1b" {
        key.name = "escape".to_owned();
        key.meta = length == 2;
    } else if s == " " || s == "\x1b " {
        key.name = "space".to_owned();
        key.meta = length == 2;
    } else if let Some(name) = ctrl_name {
        key.name = name;
        key.ctrl = true;
    } else if length == 1 && s.as_bytes()[0].is_ascii_digit() {
        key.name = s.to_owned();
        key.number = true;
    } else if length == 1 && s.as_bytes()[0].is_ascii_lowercase() {
        key.name = s.to_owned();
    } else if length == 1 && s.as_bytes()[0].is_ascii_uppercase() {
        key.name = s.to_ascii_lowercase();
        key.shift = true;
    } else if length == 1 || (length == 2 && s.chars().next().is_some_and(|c| c as u32 > 0xffff)) {
        key.name = s.to_owned();
    } else if let Some(ch) =
        s.strip_prefix('\x1b').filter(|rest| rest.len() == 1 && rest.as_bytes()[0].is_ascii_alphanumeric())
    {
        // /^(?:\x1b)([a-zA-Z0-9])$/
        key.meta = true;
        if ch == "F" {
            key.name = "right".to_owned();
        } else if ch == "B" {
            key.name = "left".to_owned();
        } else if ch.as_bytes()[0].is_ascii_uppercase() {
            key.shift = true;
            key.name = ch.to_owned();
        } else {
            key.name = ch.to_owned();
        }
    } else if let Some(name) = meta_ctrl_name {
        key.meta = true;
        key.ctrl = true;
        key.name = name;
    } else if let Some(parts) = match_fn_key(s) {
        if s.starts_with("\x1b\x1b") {
            key.option = true;
            key.meta = true;
        }
        let code: String =
            [Some(parts.introducer), parts.number, parts.terminator, parts.letter].into_iter().flatten().collect();
        let modifier_text = parts.modifier.or(parts.letter_number).unwrap_or("1");
        let modifier = to_int32(parse_int(modifier_text) - 1.0);
        set_modifier_bits(&mut key, modifier);
        key.code = Some(code.clone());
        if let Some(name) = key_name(&code) {
            key.name = name.to_owned();
            key.shift = is_shift_key(&code) || key.shift;
            key.ctrl = is_ctrl_key(&code) || key.ctrl;
            if let Some(text) = ss3_numpad_printable(&code) {
                key.sequence = text.to_owned();
                if key.name.len() == 1 && key.name.as_bytes()[0].is_ascii_digit() {
                    key.number = true;
                }
            }
        } else {
            key.name = String::new();
            key.code = None;
        }
    } else if s == "\x1b[3~" {
        key.name = "delete".to_owned();
        key.meta = false;
        key.code = Some("[3~".to_owned());
    }
    Some(key)
}

/// `parseKeypress(Buffer.from([byte]))` of a byte above 0x7F that is not a UTF-8 lead: the meta form of
/// `byte - 128`.
pub fn parse_keypress_high_byte(byte: u8, use_kitty_keyboard: bool) -> Option<ParsedKey> {
    debug_assert!(byte > 127);
    let mut text = String::from('\x1b');
    text.push((byte - 128) as char);
    parse_keypress(&text, use_kitty_keyboard)
}
