//! `extras/formatting.ts`: `formatKeySequence` and `formatCommandBindings`, with the TUI's token display,
//! key-name and modifier aliases.
use super::keys::KeySequencePart;

/// `FormatKeySequenceOptions` (`tokenDisplay` and the aliases as maps; `patternDisplay` has no patterns to name).
#[derive(Clone, Debug, Default)]
pub struct FormatOptions {
    pub token_display: Vec<(String, String)>,
    pub key_name_aliases: Option<Vec<(String, String)>>,
    pub modifier_aliases: Option<Vec<(String, String)>>,
    pub separator: Option<String>,
}

fn lookup<'a>(map: &'a [(String, String)], key: &str) -> Option<&'a str> {
    map.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str())
}

fn format_stroke(part: &KeySequencePart, options: &FormatOptions) -> String {
    if let Some(token) = &part.token_name {
        return lookup(&options.token_display, token).unwrap_or(&part.display).to_owned();
    }
    if options.key_name_aliases.is_none() && options.modifier_aliases.is_none() {
        return part.display.clone();
    }
    let stroke = &part.stroke;
    let modifier = |name: &str| {
        options.modifier_aliases.as_deref().and_then(|aliases| lookup(aliases, name)).unwrap_or(name).to_owned()
    };
    let mut pieces: Vec<String> = Vec::new();
    for (on, name) in [
        (stroke.ctrl, "ctrl"),
        (stroke.shift, "shift"),
        (stroke.meta, "meta"),
        (stroke.super_, "super"),
        (stroke.hyper, "hyper"),
    ] {
        if on {
            pieces.push(modifier(name));
        }
    }
    let name = if stroke.name == "return" { "enter" } else { &stroke.name };
    pieces
        .push(options.key_name_aliases.as_deref().and_then(|aliases| lookup(aliases, name)).unwrap_or(name).to_owned());
    pieces.join("+")
}

/// `formatKeySequence(parts, options)`: `""` for none.
pub fn format_key_sequence(parts: Option<&[KeySequencePart]>, options: &FormatOptions) -> String {
    let Some(parts) = parts.filter(|parts| !parts.is_empty()) else { return String::new() };
    let separator = options.separator.as_deref().unwrap_or(" ");
    parts.iter().map(|part| format_stroke(part, options)).collect::<Vec<_>>().join(separator)
}

/// `formatCommandBindings(bindings, options)`: the sequences joined by `, `, duplicates dropped; `None` for none.
pub fn format_command_bindings<'a>(
    sequences: impl IntoIterator<Item = &'a [KeySequencePart]>,
    options: &FormatOptions,
    binding_separator: Option<&str>,
    dedupe: bool,
) -> Option<String> {
    let mut sequences = sequences.into_iter().peekable();
    sequences.peek()?;
    let separator = binding_separator.unwrap_or(", ");
    let mut seen: Vec<String> = Vec::new();
    let mut items: Vec<String> = Vec::new();
    for sequence in sequences {
        let item = format_key_sequence(Some(sequence), options);
        if item.is_empty() {
            continue;
        }
        if dedupe {
            if seen.contains(&item) {
                continue;
            }
            seen.push(item.clone());
        }
        items.push(item);
    }
    Some(items.join(separator))
}
