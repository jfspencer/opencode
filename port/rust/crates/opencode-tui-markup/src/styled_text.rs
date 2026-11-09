//! `lib/tree-sitter-styled-text.ts`: highlights to styled chunks, with conceal.

use crate::highlight::{SimpleHighlight, SourceRange};
use crate::js::{JsStr, JsString};
use crate::marked::Nullable;
use crate::style::{create_text_attributes, StyleDefinition, StyledText, SyntaxStyle, TextChunk};
use crate::tree_sitter::TreeSitterClient;

/// `TextChunkOptions`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextChunkOptions<'a> {
    /// Conceal (default true).
    pub enabled: Option<bool>,
    pub base_highlight: Option<&'a str>,
}

fn specificity(group: &str) -> usize {
    group.split('.').count()
}

/// `shouldSuppressInInjection(group, meta)`.
fn should_suppress_in_injection(h: &SimpleHighlight) -> bool {
    if h.is_injection() {
        return false;
    }
    &*h.group == "markup.raw.block"
}

fn chunk_with(text: JsString, style: Option<&StyleDefinition>) -> TextChunk {
    TextChunk {
        text,
        fg: style.and_then(|s| s.fg),
        bg: style.and_then(|s| s.bg),
        attributes: Some(style.map_or(0, |s| create_text_attributes(s.bold, s.italic, s.underline, s.dim))),
        link: None,
    }
}

/// A merged style: `StyleDefinition` plus whether any key was set (`Object.keys(mergedStyle).length > 0`).
struct Merged {
    style: StyleDefinition,
    keyed: bool,
}

/// `treeSitterToTextChunks(content, highlights, syntaxStyle, options)`. When `ranges` is given it receives the
/// source range of each chunk, as `options.ranges` does.
pub fn tree_sitter_to_text_chunks(
    content: &[u16],
    highlights: &[SimpleHighlight],
    syntax_style: &SyntaxStyle,
    options: TextChunkOptions<'_>,
    mut ranges: Option<&mut Vec<SourceRange>>,
) -> Vec<TextChunk> {
    let mut chunks = Vec::new();
    let default_style = syntax_style.get_style("default");
    let conceal_enabled = options.enabled.unwrap_or(true);
    let base_style = options.base_highlight.and_then(|b| syntax_style.get_style(b));

    let mut injection_container_ranges: Vec<(usize, usize)> = Vec::new();
    // (offset, is_start, highlight index)
    let mut boundaries: Vec<(usize, bool, usize)> = Vec::new();
    for (i, h) in highlights.iter().enumerate() {
        if h.start == h.end {
            continue;
        }
        if h.contains_injection() {
            injection_container_ranges.push((h.start, h.end));
        }
        boundaries.push((h.start, true, i));
        boundaries.push((h.end, false, i));
    }
    boundaries.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| match (a.1, b.1) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        })
    });

    // A Set in insertion order.
    let mut active: Vec<usize> = Vec::new();
    let mut current = 0usize;
    let mut i = 0;
    while i < boundaries.len() {
        let (offset, is_start, index) = boundaries[i];
        if current < offset && !active.is_empty() {
            let segment = content.js_slice(current as isize, Some(offset as isize));
            let conceal = if conceal_enabled {
                active.iter().map(|&a| &highlights[a]).find(|h| {
                    *h.conceal() != Nullable::Undefined || &*h.group == "conceal" || h.group.starts_with("conceal.")
                })
            } else {
                None
            };
            if let Some(ch) = conceal {
                let replacement: Option<JsString> = match ch.conceal() {
                    Nullable::Str(s) => Some(s.clone()),
                    Nullable::Null => None,
                    Nullable::Undefined => (&*ch.group == "conceal.with.space").then(|| JsString::from(" ")),
                };
                if let Some(text) = replacement.filter(|t| !t.is_empty()) {
                    chunks.push(chunk_with(text, default_style.as_ref()));
                    if let Some(r) = ranges.as_deref_mut() {
                        r.push(SourceRange { start: current, end: offset });
                    }
                }
            } else {
                let inside_container = injection_container_ranges.iter().any(|&(s, e)| current >= s && current < e);
                let mut valid: Vec<usize> = active
                    .iter()
                    .copied()
                    .filter(|&a| !(inside_container && should_suppress_in_injection(&highlights[a])))
                    .collect();
                valid.sort_by(|&a, &b| {
                    specificity(&highlights[a].group).cmp(&specificity(&highlights[b].group)).then(a.cmp(&b))
                });
                let mut merged = match base_style {
                    Some(b) => Merged { style: b, keyed: true },
                    None => Merged { style: StyleDefinition::default(), keyed: false },
                };
                for &a in &valid {
                    let group = &*highlights[a].group;
                    let mut style = syntax_style.get_style(group);
                    if style.is_none() && group.contains('.') {
                        style = syntax_style.get_style(group.split('.').next().unwrap());
                    }
                    if let Some(s) = style {
                        if s.fg.is_some() {
                            merged.style.fg = s.fg;
                            merged.keyed = true;
                        }
                        if s.bg.is_some() {
                            merged.style.bg = s.bg;
                            merged.keyed = true;
                        }
                        if s.bold.is_some() {
                            merged.style.bold = s.bold;
                            merged.keyed = true;
                        }
                        if s.italic.is_some() {
                            merged.style.italic = s.italic;
                            merged.keyed = true;
                        }
                        if s.underline.is_some() {
                            merged.style.underline = s.underline;
                            merged.keyed = true;
                        }
                        if s.dim.is_some() {
                            merged.style.dim = s.dim;
                            merged.keyed = true;
                        }
                    }
                }
                let final_style = if merged.keyed { Some(merged.style) } else { default_style };
                chunks.push(chunk_with(segment, final_style.as_ref()));
                if let Some(r) = ranges.as_deref_mut() {
                    r.push(SourceRange { start: current, end: offset });
                }
            }
        } else if current < offset {
            let text = content.js_slice(current as isize, Some(offset as isize));
            chunks.push(chunk_with(text, base_style.as_ref().or(default_style.as_ref())));
            if let Some(r) = ranges.as_deref_mut() {
                r.push(SourceRange { start: current, end: offset });
            }
        }

        if is_start {
            active.push(index);
        } else {
            active.retain(|&a| a != index);
            if conceal_enabled {
                let h = &highlights[index];
                if *h.conceal_lines() != Nullable::Undefined && content.get(offset) == Some(&(b'\n' as u16)) {
                    current = offset + 1;
                    i += 1;
                    continue;
                }
                match h.conceal() {
                    Nullable::Undefined => {}
                    Nullable::Str(c) if *c == " " => {
                        if content.get(offset) == Some(&(b' ' as u16)) {
                            current = offset + 1;
                            i += 1;
                            continue;
                        }
                    }
                    conceal => {
                        let empty = matches!(conceal, Nullable::Str(c) if c.is_empty());
                        if empty && &*h.group == "conceal" && !h.is_injection() && content.get(offset) == Some(&(b' ' as u16)) {
                            current = offset + 1;
                            i += 1;
                            continue;
                        }
                    }
                }
            }
        }
        current = offset;
        i += 1;
    }

    if current < content.len() {
        let text = content.js_slice(current as isize, None);
        chunks.push(chunk_with(text, base_style.as_ref().or(default_style.as_ref())));
        if let Some(r) = ranges.as_deref_mut() {
            r.push(SourceRange { start: current, end: content.len() });
        }
    }
    chunks
}

/// `treeSitterToStyledText(content, filetype, syntaxStyle, client, options)`.
pub fn tree_sitter_to_styled_text(
    content: &JsString,
    filetype: &str,
    syntax_style: &SyntaxStyle,
    client: &TreeSitterClient,
    conceal: Option<bool>,
    base_highlight: Option<&str>,
) -> StyledText {
    let result = client.highlight_once(content, filetype);
    let highlights = result.highlights.unwrap_or_default();
    if !highlights.is_empty() || base_highlight.is_some() {
        let chunks = tree_sitter_to_text_chunks(
            content,
            &highlights,
            syntax_style,
            TextChunkOptions { enabled: Some(conceal.unwrap_or(true)), base_highlight },
            None,
        );
        StyledText::new(chunks)
    } else {
        let merged = syntax_style.merge_styles(&["default"]);
        StyledText::new(vec![TextChunk {
            text: content.clone(),
            fg: merged.fg,
            bg: merged.bg,
            attributes: Some(merged.attributes),
            link: None,
        }])
    }
}
