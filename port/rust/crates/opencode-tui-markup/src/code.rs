//! `Code.ts`'s highlight pipeline, without the renderable: what one `startHighlight` pass computes from the
//! content, filetype and options — the chunks it hands the text buffer and the rendered-line source map
//! (`getConcealLinesSourceMap`). The renderable's state machine (snapshot ids, reruns, `drawUnstyledText`,
//! streaming) stays with the renderer, which runs [`highlight`] as its `highlightOnce` await.

use crate::highlight::{SimpleHighlight, SourceRange};
use crate::js::{JsStr, JsString};
use crate::marked::Nullable;
use crate::style::{StyledText, SyntaxStyle, TextChunk};
use crate::styled_text::{tree_sitter_to_text_chunks, TextChunkOptions};
use crate::tree_sitter::TreeSitterClient;

/// `HighlightContext` / `ChunkRenderContext` as the hooks see them.
pub struct ChunkContext<'a> {
    pub content: &'a JsString,
    pub filetype: &'a str,
    pub syntax_style: &'a SyntaxStyle,
    pub highlights: &'a [SimpleHighlight],
    pub source_ranges: Option<&'a [SourceRange]>,
}

/// The `onHighlight` hook: may replace the highlights.
pub type OnHighlight<'a> = &'a dyn Fn(&[SimpleHighlight], &JsString, &str) -> Option<Vec<SimpleHighlight>>;
/// The `onChunks` hook: may replace the chunks.
pub type OnChunks<'a> = &'a dyn Fn(Vec<TextChunk>, &ChunkContext<'_>) -> Vec<TextChunk>;

/// The options of one highlight pass (the CodeRenderable props it reads).
pub struct HighlightOptions<'a> {
    pub conceal: bool,
    pub base_highlight: Option<&'a str>,
    pub on_highlight: Option<OnHighlight<'a>>,
    pub on_chunks: Option<OnChunks<'a>>,
}

/// What a pass sets on the text buffer.
#[derive(Clone, Debug, PartialEq)]
pub enum HighlightOutcome {
    /// `textBuffer.setStyledText(new StyledText(chunks))` and the rendered-line sources (`None`: identity).
    Styled { text: StyledText, rendered_line_sources: Option<Vec<usize>> },
    /// `textBuffer.setText(content)`: no highlights and no hooks (or the TS fallback).
    Plain,
}

/// `startHighlight`'s computation for `content` in `filetype`. The renderable runs it only for non-empty content
/// with a filetype (`scheduleHighlights` otherwise shows the plain text, or nothing for empty content).
pub fn highlight(
    client: &TreeSitterClient,
    content: &JsString,
    filetype: &str,
    syntax_style: &SyntaxStyle,
    options: &HighlightOptions<'_>,
) -> HighlightOutcome {
    let result = client.highlight_once(content, filetype);
    let mut highlights = result.highlights.unwrap_or_default();
    if let Some(hook) = options.on_highlight {
        if let Some(modified) = hook(&highlights, content, filetype) {
            highlights = modified;
        }
    }
    if highlights.is_empty() && options.on_chunks.is_none() && options.base_highlight.is_none() {
        return HighlightOutcome::Plain;
    }
    let mut ranges = options.on_chunks.map(|_| Vec::new());
    let chunks = tree_sitter_to_text_chunks(
        content,
        &highlights,
        syntax_style,
        TextChunkOptions { enabled: Some(options.conceal), base_highlight: options.base_highlight },
        ranges.as_mut(),
    );
    let rendered_line_sources =
        if options.on_chunks.is_some() { None } else { get_conceal_lines_source_map(options.conceal, content, &highlights) };
    let chunks = match options.on_chunks {
        Some(hook) => hook(
            chunks,
            &ChunkContext {
                content,
                filetype,
                syntax_style,
                highlights: &highlights,
                source_ranges: ranges.as_deref(),
            },
        ),
        None => chunks,
    };
    HighlightOutcome::Styled { text: StyledText::new(chunks), rendered_line_sources }
}

/// `CodeRenderable.getMergedConcealLineRanges(highlights)`.
pub fn get_merged_conceal_line_ranges(highlights: &[SimpleHighlight]) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for h in highlights {
        if *h.conceal_lines() == Nullable::Undefined {
            continue;
        }
        let empty = match h.conceal() {
            Nullable::Str(s) => s.is_empty(),
            Nullable::Undefined => &*h.group == "conceal" || h.group.starts_with("conceal."),
            Nullable::Null => false,
        };
        if empty {
            ranges.push((h.start, h.end));
        }
    }
    if ranges.len() <= 1 {
        return ranges;
    }
    ranges.sort_by_key(|r| r.0);
    let mut write = 0;
    for i in 1..ranges.len() {
        let next = ranges[i];
        if next.0 <= ranges[write].1 {
            ranges[write].1 = ranges[write].1.max(next.1);
        } else {
            write += 1;
            ranges[write] = next;
        }
    }
    ranges.truncate(write + 1);
    ranges
}

/// `getConcealLinesSourceMap(content, highlights)`: rendered line → source line, when concealed lines shift them.
pub fn get_conceal_lines_source_map(conceal: bool, content: &[u16], highlights: &[SimpleHighlight]) -> Option<Vec<usize>> {
    if !conceal || content.is_empty() {
        return None;
    }
    let ranges = get_merged_conceal_line_ranges(highlights);
    if ranges.is_empty() {
        return None;
    }
    let mut sources: Vec<usize> = Vec::new();
    let mut source_line = 0;
    let mut line_start = 0usize;
    let mut range_index = 0;
    let mut current_has_text = false;
    let set_current = |sources: &mut Vec<usize>, current_has_text: &mut bool, line: usize, has_text: bool| {
        if sources.is_empty() {
            sources.push(line);
        } else if !*current_has_text {
            *sources.last_mut().unwrap() = line;
        }
        if has_text {
            *current_has_text = true;
        }
    };
    while line_start <= content.len() {
        let newline = content.index_of(&[b'\n' as u16], line_start);
        let line_end = if newline == -1 { content.len() } else { newline as usize };
        while range_index < ranges.len() && ranges[range_index].1 <= line_start {
            range_index += 1;
        }
        let range = ranges.get(range_index);
        let fully = range.is_some_and(|r| line_end > line_start && r.0 <= line_start && r.1 >= line_end);
        let break_concealed =
            newline != -1 && range.is_some_and(|r| r.0 <= newline as usize && r.1 >= newline as usize);
        if !fully || !break_concealed {
            let has_text = line_end > line_start && !fully;
            if has_text || newline != -1 || !fully {
                set_current(&mut sources, &mut current_has_text, source_line, has_text);
            }
            if newline != -1 && !break_concealed {
                sources.push(source_line + 1);
                current_has_text = false;
            }
        }
        source_line += 1;
        if newline == -1 {
            break;
        }
        line_start = newline as usize + 1;
    }
    if sources.is_empty() || sources.iter().enumerate().all(|(i, &s)| s == i) {
        return None;
    }
    Some(sources)
}
