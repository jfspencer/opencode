//! `lib/detect-links.ts` (`detectLinks`, `normalizeMarkdownLinkTarget`) and `Markdown.ts`'s
//! `addMarkdownLinkHighlights`, the `onChunks` and `onHighlight` hooks of markdown text blocks.

use crate::highlight::{HighlightMeta, SimpleHighlight, SourceRange};
use crate::js::{is_js_whitespace, JsRegex, JsStr, JsString};
use crate::marked::Nullable;
use crate::markdown::inline::is_supported_link_target;
use crate::style::{SyntaxStyle, TextChunk};
use std::collections::HashMap;
use std::sync::LazyLock;

static LINK_TARGET_ESCAPE: LazyLock<JsRegex> =
    LazyLock::new(|| JsRegex::compile(r##"\\([!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~])"##, "g"));
static HTTP_URL_PREFIX: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"https?:\/\/", "i"));
static BARE_URL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r#"https?:\/\/[^\s<>"`]+"#, "gi"));
static DELIMITERS: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::compile(r"^[*_~]+$", ""));

fn is_url_scope(group: &str) -> bool {
    group == "markup.link.url" || group == "string.special.url"
}

/// `normalizeMarkdownLinkTarget(destination)`.
pub fn normalize_markdown_link_target(destination: &[u16]) -> JsString {
    LINK_TARGET_ESCAPE.replace_str(destination, "$1")
}

fn unit_is(content: &[u16], i: isize, c: u8) -> bool {
    i >= 0 && content.get(i as usize) == Some(&(c as u16))
}

/// `addMarkdownLinkHighlights(highlights, content)` with the renderable's `conceal` and hyperlink support.
pub fn add_markdown_link_highlights(
    highlights: &[SimpleHighlight],
    content: &[u16],
    conceal: bool,
    hyperlinks: bool,
) -> Vec<SimpleHighlight> {
    if !highlights.iter().any(|h| is_url_scope(&h.group)) {
        return highlights.to_vec();
    }
    let mut modified = highlights.to_vec();
    let mut labels: HashMap<usize, &SimpleHighlight> = HashMap::new();
    let mut bracket_conceals: HashMap<usize, usize> = HashMap::new();
    let mut closings: Vec<&SimpleHighlight> = Vec::new();
    for (index, h) in highlights.iter().enumerate() {
        if &*h.group == "markup.link.label" && h.start < h.end && !labels.contains_key(&h.end) {
            labels.insert(h.end, h);
        }
        if h.end == h.start + 1 && h.conceal().as_str().is_some_and(|c| c == " ") && !bracket_conceals.contains_key(&h.start) {
            bracket_conceals.insert(h.start, index);
        }
        if &*h.group == "markup.link" && content.js_slice(h.start as isize, Some(h.end as isize)) == ")" {
            closings.push(h);
        }
    }
    let concealed = |start: usize, end: usize| {
        SimpleHighlight::new(
            start,
            end,
            "conceal",
            Some(HighlightMeta { conceal: Nullable::Str(JsString::new()), is_injection: Some(true), ..Default::default() }),
        )
    };
    let mut closing_index = 0;
    for (index, h) in highlights.iter().enumerate() {
        if !is_url_scope(&h.group) {
            continue;
        }
        let (start, end) = (h.start, h.end);
        let (mut url_start, mut url_end) = (start, end);
        if unit_is(content, start as isize, b'<') && unit_is(content, end as isize - 1, b'>') {
            url_start += 1;
            url_end -= 1;
            modified[index] = SimpleHighlight::new(url_start, url_end, h.group.clone(), h.meta.clone());
            if conceal {
                modified.push(concealed(start, start + 1));
                modified.push(concealed(end - 1, end));
            }
        }
        let mut destination_start = start;
        while destination_start > 0 && is_js_whitespace(content[destination_start - 1]) {
            destination_start -= 1;
        }
        let ds = destination_start as isize;
        if !conceal || !unit_is(content, ds - 1, b'(') || !unit_is(content, ds - 2, b']') {
            continue;
        }
        let Some(label) = labels.get(&(destination_start - 2)) else { continue };
        let destination = content.js_slice(url_start as isize, Some(url_end as isize));
        let url = if &*h.group == "markup.link.url" { normalize_markdown_link_target(&destination) } else { destination };
        if (!hyperlinks || !is_supported_link_target(&url))
            && content.js_slice(label.start as isize, Some(label.end as isize)) != url
        {
            continue;
        }
        while closing_index < closings.len() && closings[closing_index].start < end {
            closing_index += 1;
        }
        let Some(close) = closings.get(closing_index) else { continue };
        if let Some(&bi) = bracket_conceals.get(&(destination_start - 2)) {
            let b = &modified[bi];
            let mut meta = b.meta.clone().unwrap_or_default();
            meta.conceal = Nullable::Str(JsString::new());
            modified[bi] = SimpleHighlight::new(b.start, b.end, b.group.clone(), Some(meta));
        }
        modified.push(concealed(destination_start - 1, close.end));
    }
    modified
}

struct LinkRange {
    start: usize,
    end: usize,
    url: JsString,
    label: bool,
    bare: bool,
}

/// `detectLinks(chunks, context)`: attaches `link` to chunks that show a URL or a link label, splitting chunks at
/// link boundaries and styling bare URLs as `markup.link.label`.
pub fn detect_links(
    chunks: Vec<TextChunk>,
    content: &[u16],
    highlights: &[SimpleHighlight],
    source_ranges: Option<&[SourceRange]>,
    syntax_style: Option<&SyntaxStyle>,
) -> Vec<TextChunk> {
    let has_bare_url = HTTP_URL_PREFIX.test(content);
    if !has_bare_url && !highlights.iter().any(|h| is_url_scope(&h.group)) {
        return chunks;
    }
    let mut ranges: Vec<LinkRange> = Vec::new();
    let mut raw_ranges: Vec<(usize, usize)> = Vec::new();
    let mut labels: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut concealed_delimiters: HashMap<usize, usize> = HashMap::new();
    for h in highlights {
        let (start, end, group) = (h.start, h.end, &*h.group);
        if group == "conceal"
            && h.conceal().as_str().is_some_and(|c| c.is_empty())
            && DELIMITERS.test(&content.js_slice(start as isize, Some(end as isize)))
        {
            concealed_delimiters.insert(end, start);
        }
        if group == "markup.raw" || group.starts_with("markup.raw.") {
            raw_ranges.push((start, end));
        }
        if group == "markup.link.label" {
            let existing = labels.get(&end);
            if existing.is_none_or(|e| start < e.0) {
                labels.insert(end, (start, end));
            }
        }
        if !is_url_scope(group) {
            continue;
        }
        let url = content.js_slice(start as isize, Some(end as isize));
        if url.is_empty() {
            continue;
        }
        ranges.push(LinkRange { start, end, url, label: false, bare: false });
    }

    let highlighted_count = ranges.len();
    for index in 0..highlighted_count {
        let mut label_end = ranges[index].start as isize;
        if unit_is(content, label_end - 1, b'<') {
            label_end -= 1;
        }
        while label_end > 0 && is_js_whitespace(content[label_end as usize - 1]) {
            label_end -= 1;
        }
        if !unit_is(content, label_end - 1, b'(') || !unit_is(content, label_end - 2, b']') {
            continue;
        }
        let Some(&(ls, le)) = labels.get(&((label_end - 2) as usize)) else { continue };
        let url = normalize_markdown_link_target(&ranges[index].url);
        ranges[index].url = url.clone();
        ranges.push(LinkRange { start: ls, end: le, url, label: true, bare: false });
    }

    if has_bare_url {
        raw_ranges.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        ranges.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        let mut raw_index = 0;
        let mut highlighted_index = 0;
        for m in BARE_URL.find_all(content) {
            let start = m.start;
            let mut url = JsString(content[m.start..m.end].to_vec());
            let mut open = 0i64;
            let mut close = 0i64;
            for c in char::decode_utf16(url.iter().copied()) {
                match c {
                    Ok('(') => open += 1,
                    Ok(')') => close += 1,
                    _ => {}
                }
            }
            while !url.is_empty() {
                if let Some(&delimiter_start) = concealed_delimiters.get(&(start + url.len())) {
                    if delimiter_start >= start {
                        url = url.js_slice(0, Some((delimiter_start - start) as isize));
                        continue;
                    }
                }
                let last = *url.last().unwrap();
                if last == b')' as u16 && close > open {
                    close -= 1;
                } else if !".,!?;:]}'".encode_utf16().any(|c| c == last) {
                    break;
                }
                url = url.js_slice(0, Some(-1));
            }
            let end = start + url.len();
            if url.is_empty() {
                continue;
            }
            while raw_index < raw_ranges.len() && raw_ranges[raw_index].1 <= start {
                raw_index += 1;
            }
            if raw_index < raw_ranges.len() && raw_ranges[raw_index].0 < end {
                continue;
            }
            while highlighted_index < ranges.len() && ranges[highlighted_index].end <= start {
                highlighted_index += 1;
            }
            if highlighted_index < ranges.len() && ranges[highlighted_index].start < end {
                continue;
            }
            ranges.push(LinkRange { start, end, url, label: false, bare: true });
        }
    }

    if ranges.is_empty() {
        return chunks;
    }
    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)).then((b.label as i32).cmp(&(a.label as i32))));

    let bare_style = syntax_style.map(|s| s.merge_styles(&["markup.link.label"]));
    let mut linked: Vec<TextChunk> = Vec::with_capacity(chunks.len());
    let mut content_pos = 0usize;
    let mut range_index = 0usize;
    for (index, mut chunk) in chunks.into_iter().enumerate() {
        if chunk.text.is_empty() || chunk.link.is_some() {
            linked.push(chunk);
            continue;
        }
        let provided = source_ranges.and_then(|r| r.get(index));
        let start = match provided {
            Some(r) => r.start as isize,
            None => content.index_of(&chunk.text, content_pos),
        };
        if start < 0 {
            linked.push(chunk);
            continue;
        }
        let start = start as usize;
        let end = provided.map_or(start + chunk.text.len(), |r| r.end);
        content_pos = end;
        while range_index < ranges.len() && ranges[range_index].end <= start {
            range_index += 1;
        }
        if content.js_slice(start as isize, Some(end as isize)) != chunk.text {
            if let Some(range) = ranges.get(range_index) {
                if range.label && range.start < end {
                    chunk.link = Some(range.url.clone());
                }
            }
            linked.push(chunk);
            continue;
        }
        let mut position = start;
        while range_index < ranges.len() {
            let range = &ranges[range_index];
            if range.end <= position {
                range_index += 1;
                continue;
            }
            if range.start >= end {
                break;
            }
            let link_start = position.max(range.start);
            let link_end = end.min(range.end);
            if link_start > position {
                linked.push(TextChunk {
                    text: chunk.text.js_slice((position - start) as isize, Some((link_start - start) as isize)),
                    ..chunk.clone()
                });
            }
            let mut piece = if link_start == start && link_end == end {
                chunk.clone()
            } else {
                TextChunk {
                    text: chunk.text.js_slice((link_start - start) as isize, Some((link_end - start) as isize)),
                    ..chunk.clone()
                }
            };
            piece.link = Some(range.url.clone());
            if range.bare {
                if let Some(bare) = bare_style {
                    if bare.fg.is_some() {
                        piece.fg = bare.fg;
                    }
                    if bare.bg.is_some() {
                        piece.bg = bare.bg;
                    }
                    piece.attributes = Some(piece.attributes.unwrap_or(0) | bare.attributes);
                }
            }
            let whole = link_start == start && link_end == end;
            linked.push(piece);
            position = link_end;
            if whole {
                // TS mutated the chunk itself: later pieces (none remain) would see its link.
                chunk.link = Some(range.url.clone());
            }
            if position == end {
                break;
            }
            range_index += 1;
        }
        if position == start {
            linked.push(chunk);
        } else if position < end {
            linked.push(TextChunk { text: chunk.text.js_slice((position - start) as isize, None), ..chunk });
        }
    }
    linked
}
