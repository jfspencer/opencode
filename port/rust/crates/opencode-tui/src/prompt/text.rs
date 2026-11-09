//! The prompt's text helpers: `prompt/parse.ts`, `prompt/display.ts`, `prompt/part.ts`, `prompt/content.ts`,
//! `prompt/traits.ts`, and the `Locale` and string helpers they and the prompt use (`truncateMiddle`, `truncate`,
//! JS `\s` and `trim`).
//!
//! Text indices are byte offsets of the Rust strings where TS uses UTF-16 indices: they are only used to slice the
//! same string, so the results are the same. Editor offsets (`measureEditText`) are cells plus one per line break.
use opencode_tui_text::WidthMethod;

/// JS `\s`.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}'
            | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `String.prototype.trim`.
pub fn trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `/\s/.test(text)`.
pub fn has_whitespace(text: &str) -> bool {
    text.chars().any(is_js_whitespace)
}

/// `measureEditText(text, { widthMethod })`: the editor offset at the end of `text`.
pub fn measure(text: &str, width_method: WidthMethod) -> i64 {
    opencode_tui_text::measure_edit_text(text, width_method, 0).unwrap_or(0) as i64
}

/// JS `str.length` (UTF-16 code units).
pub fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `str.slice(start, end)` over UTF-16 code units (unpaired halves become U+FFFD).
pub fn js_slice(text: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = end.min(units.len());
    let start = start.min(end);
    String::from_utf16_lossy(&units[start..end])
}

/// `Locale.truncate(str, len)`.
pub fn truncate(text: &str, len: usize) -> String {
    if js_len(text) <= len {
        return text.to_string();
    }
    js_slice(text, 0, len.saturating_sub(1)) + "…"
}

/// `Locale.truncateMiddle(str, maxLength)`.
pub fn truncate_middle(text: &str, max_length: i64) -> String {
    if max_length <= 0 {
        return String::new();
    }
    let len = js_len(text);
    if len as i64 <= max_length {
        return text.to_string();
    }
    let keep_start = ((max_length - 1) as f64 / 2.0).ceil() as usize;
    let keep_end = ((max_length - 1) as f64 / 2.0).floor() as i64;
    if keep_end <= 0 {
        return js_slice(text, 0, keep_start) + "…";
    }
    js_slice(text, 0, keep_start) + "…" + &js_slice(text, len - keep_end as usize, len)
}

/// `a.localeCompare(b)` for the ASCII text of command names and paths: CLDR root collation's primary order
/// (whitespace, punctuation in its table order, digits, letters ignoring case), then lowercase before uppercase.
/// TODO(port): the full ICU collation is the server crate's (`collate::compare`); non-ASCII falls back to code
/// points here.
pub fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    const PUNCTUATION: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";
    fn primary(c: char) -> (u32, u32) {
        if c.is_whitespace() {
            return (0, c as u32);
        }
        if let Some(i) = PUNCTUATION.find(c) {
            return (1, i as u32);
        }
        if c.is_ascii_digit() {
            return (2, c as u32);
        }
        if c.is_ascii_alphabetic() {
            return (3, c.to_ascii_lowercase() as u32);
        }
        (4, c as u32)
    }
    let left: Vec<(u32, u32)> = a.chars().map(primary).collect();
    let right: Vec<(u32, u32)> = b.chars().map(primary).collect();
    left.cmp(&right).then_with(|| {
        let tertiary = |s: &str| s.chars().map(|c| c.is_ascii_uppercase()).collect::<Vec<_>>();
        tertiary(a).cmp(&tertiary(b))
    })
}

// -- prompt/parse.ts ---------------------------------------------------------------------------------------------

/// A line range of a file mention (`#12` or `#12-20`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineRange {
    pub start_line: u64,
    pub end_line: Option<u64>,
}

/// `parseFileLineRange(input)`: the base and the range after its last `#`.
pub fn parse_file_line_range(input: &str) -> (String, Option<LineRange>) {
    let Some(hash) = input.rfind('#').filter(|h| *h > 0) else { return (input.to_string(), None) };
    let rest = &input[hash + 1..];
    let (start, end) = match rest.split_once('-') {
        Some((start, end)) => (start, Some(end)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(start) || end.is_some_and(|e| !e.is_empty() && !digits(e)) {
        return (input.to_string(), None);
    }
    let start_line: f64 = start.parse().unwrap_or(f64::NAN);
    let end_line = end.filter(|e| !e.is_empty()).map(|e| e.parse::<f64>().unwrap_or(f64::NAN)).filter(|e| start_line < *e);
    (input[..hash].to_string(), Some(LineRange { start_line: start_line as u64, end_line: end_line.map(|e| e as u64) }))
}

/// `stripFileLineRange(input)`.
pub fn strip_file_line_range(input: &str) -> String {
    parse_file_line_range(input).0
}

/// `parseSlashHead`'s result.
#[derive(Clone, Debug, PartialEq)]
pub struct SlashHead {
    pub name: String,
    pub arguments: String,
    /// Where the name ends (byte offset).
    pub end: usize,
}

/// `parseSlashHead(text, separator)`: `whitespace` is `/\s/`, else `/[ \t\n]/`.
pub fn parse_slash_head(text: &str, whitespace: bool) -> Option<SlashHead> {
    let rest = text.strip_prefix('/')?;
    let separator = |c: char| if whitespace { is_js_whitespace(c) } else { matches!(c, ' ' | '\t' | '\n') };
    let Some(end) = rest.find(separator) else {
        return Some(SlashHead { name: rest.to_string(), arguments: String::new(), end: text.len() });
    };
    let split = end + 1;
    let sep_len = rest[end..].chars().next().map_or(1, char::len_utf8);
    Some(SlashHead { name: text[1..split].to_string(), arguments: text[split + sep_len..].to_string(), end: split })
}

// -- prompt/display.ts -------------------------------------------------------------------------------------------

/// The grapheme boundaries of `value` (byte offsets), with its end.
fn grapheme_boundaries(value: &str) -> Vec<usize> {
    let mut boundaries: Vec<usize> = Vec::new();
    let mut offset = 0;
    for g in crate::util::graphemes(value) {
        boundaries.push(offset);
        offset += g.len();
    }
    boundaries.push(value.len());
    boundaries
}

/// `promptOffsetIndex(value, offset, widthMethod)`: the last grapheme boundary of `value` whose editor offset is at
/// most `offset`.
pub fn prompt_offset_index(value: &str, offset: i64, width_method: WidthMethod) -> usize {
    let boundaries = grapheme_boundaries(value);
    let mut low = 0usize;
    let mut high = boundaries.len() - 1;
    while low < high {
        let middle = (low + high).div_ceil(2);
        if measure(&value[..boundaries[middle]], width_method) > offset {
            high = middle - 1;
            continue;
        }
        low = middle;
    }
    boundaries[low]
}

/// `mentionTriggerIndex(text, widthMethod)`: the editor offset of the `@` a mention query starts at.
pub fn mention_trigger_index(text: &str, width_method: WidthMethod) -> Option<i64> {
    let index = text.rfind('@')?;
    let before = text[..index].chars().next_back();
    let query = &text[index..];
    if before.is_none_or(is_js_whitespace) && !has_whitespace(query) {
        return Some(measure(&text[..index], width_method));
    }
    None
}

/// `slashTriggerIndex(text, widthMethod)`.
pub fn slash_trigger_index(text: &str, width_method: WidthMethod) -> Option<i64> {
    let mut search = text.len();
    while let Some(index) = text[..search].rfind('/') {
        let before = text[..index].chars().next_back();
        let query = &text[index..];
        if before.is_some_and(|c| !is_js_whitespace(c)) {
            if index == 0 {
                return None;
            }
            search = index;
            continue;
        }
        if has_whitespace(query) || query[1..].contains('/') {
            return None;
        }
        return Some(measure(&text[..index], width_method));
    }
    None
}

// -- prompt/part.ts ----------------------------------------------------------------------------------------------

/// `expandPastedTextPlaceholders(text, pasted)`: each placeholder found in order replaced by its text.
pub fn expand_pasted_text_placeholders(text: &str, pasted: &[crate::prompt::history::PastedText]) -> String {
    let mut sorted: Vec<&crate::prompt::history::PastedText> = pasted.iter().collect();
    sorted.sort_by_key(|p| p.source.start);
    let mut cursor = 0usize;
    let mut result = String::new();
    for part in sorted {
        let Some(found) = text[cursor..].find(&part.source.text) else { continue };
        let index = cursor + found;
        result.push_str(&text[cursor..index]);
        result.push_str(&part.text);
        cursor = index + part.source.text.len();
    }
    result + &text[cursor..]
}

/// A tracked range of the prompt and its replacement.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackedText {
    pub start: i64,
    pub end: i64,
    pub text: String,
}

/// `expandTrackedPastedText(text, ranges, widthMethod)`: ranges expand right to left, so indices found in the
/// unexpanded text stay valid.
pub fn expand_tracked_pasted_text(text: &str, ranges: &[TrackedText], width_method: WidthMethod) -> String {
    let mut sorted: Vec<&TrackedText> = ranges.iter().collect();
    // A stable sort, descending, as `toSorted((a, b) => b.start - a.start)`.
    sorted.sort_by(|a, b| b.start.cmp(&a.start));
    sorted.into_iter().fold(text.to_string(), |result, part| {
        let start = prompt_offset_index(text, part.start, width_method);
        let end = prompt_offset_index(text, part.end, width_method);
        format!("{}{}{}", &result[..start.min(result.len())], part.text, result.get(end..).unwrap_or(""))
    })
}

// -- prompt/content.ts -------------------------------------------------------------------------------------------

/// `normalizePromptContent(content)`: one trailing line break of a single line dropped.
pub fn normalize_prompt_content(content: &str) -> String {
    for ending in ["\r\n", "\n"] {
        if let Some(body) = content.strip_suffix(ending) {
            if !body.contains('\n') && !body.contains('\r') {
                return body.to_string();
            }
            return content.to_string();
        }
    }
    content.to_string()
}

// -- prompt/traits.ts --------------------------------------------------------------------------------------------

/// `computePromptTraits({ mode, autocompleteVisible })` (the keymap's suspend is kept as it is).
pub fn compute_prompt_traits(shell: bool, autocomplete_visible: bool) -> (Option<Vec<&'static str>>, Option<String>) {
    let capture = if shell {
        None
    } else if autocomplete_visible {
        Some(vec!["escape", "navigate", "submit", "tab"])
    } else {
        Some(vec!["tab"])
    };
    (capture, shell.then(|| "SHELL".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_heads() {
        assert_eq!(parse_slash_head("/greet Bob", true), Some(SlashHead { name: "greet".into(), arguments: "Bob".into(), end: 6 }));
        assert_eq!(parse_slash_head("/greet", true), Some(SlashHead { name: "greet".into(), arguments: "".into(), end: 6 }));
        assert_eq!(parse_slash_head("greet", true), None);
    }

    #[test]
    fn line_ranges() {
        assert_eq!(parse_file_line_range("guide.md#2-3"), ("guide.md".into(), Some(LineRange { start_line: 2, end_line: Some(3) })));
        assert_eq!(parse_file_line_range("guide.md#5-3"), ("guide.md".into(), Some(LineRange { start_line: 5, end_line: None })));
        assert_eq!(parse_file_line_range("guide.md#2-"), ("guide.md".into(), Some(LineRange { start_line: 2, end_line: None })));
        assert_eq!(parse_file_line_range("guide.md#x"), ("guide.md#x".into(), None));
        assert_eq!(parse_file_line_range("#2"), ("#2".into(), None));
    }

    #[test]
    fn triggers() {
        let wm = WidthMethod::Unicode;
        assert_eq!(mention_trigger_index("Look at @mai", wm), Some(8));
        assert_eq!(mention_trigger_index("mail me@exam", wm), None);
        assert_eq!(slash_trigger_index("/se", wm), Some(0));
        assert_eq!(slash_trigger_index("/se x", wm), None);
        assert_eq!(slash_trigger_index("a/b", wm), None);
        assert_eq!(truncate_middle("abcdefghij", 5), "ab…ij");
    }
}
