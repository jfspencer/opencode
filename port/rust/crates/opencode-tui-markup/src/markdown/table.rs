//! `Markdown.ts`'s table content: cell chunks (`createTableHeaderCellChunks`, `createTableDataCellChunks`), the
//! cell keys (`hashString`, `hashTableToken`, `getTableCellKey`) and `buildTableContentCache`.

use super::inline::InlineContext;
use crate::marked::{TableCell, Token, TokenType};
use crate::style::{create_text_attributes, TextChunk};
use crate::js::JsString;

/// A table cell's content (`TextTableCellContent`): chunks, or `None` for null/undefined.
pub type TextTableCellContent = Option<Vec<TextChunk>>;
/// `TextTableContent`: rows of cells.
pub type TextTableContent = Vec<Vec<TextTableCellContent>>;

/// `TableContentCache`.
#[derive(Clone, Debug, PartialEq)]
pub struct TableContentCache {
    pub content: TextTableContent,
    pub cell_keys: Vec<Vec<u32>>,
}

/// `hashString(value, seed)`: FNV-1a over UTF-16 code units.
pub fn hash_string(value: &[u16], seed: u32) -> u32 {
    let mut hash = seed;
    for &c in value {
        hash ^= c as u32;
        hash = hash.wrapping_mul(16777619);
    }
    hash
}

/// `hashTableToken(token, seed)`: every marked token has `raw`, so this is the hash of its type then raw.
pub fn hash_table_token(token: &Token, seed: u32) -> u32 {
    let hash = hash_string(&JsString::from(token.kind.as_str()), seed);
    hash_string(&token.raw, hash)
}

/// `getTableCellKey(cell, isHeader)`.
pub fn get_table_cell_key(cell: Option<&TableCell>, is_header: bool) -> u32 {
    let seed: u32 = if is_header { 2902232141 } else { 1371922141 };
    match cell {
        None => seed,
        // marked cells always carry `text`.
        Some(cell) => hash_string(&cell.text, seed),
    }
}

/// `createTableDataCellChunks(cell)`.
pub fn create_table_data_cell_chunks(ctx: &InlineContext<'_>, cell: Option<&TableCell>) -> Vec<TextChunk> {
    let mut chunks = Vec::new();
    if let Some(cell) = cell {
        ctx.render_inline_content(&cell.tokens, &mut chunks, None);
    }
    if chunks.is_empty() { vec![ctx.create_default_chunk(JsString::from(" "), None)] } else { chunks }
}

/// `createTableHeaderCellChunks(cell)`.
pub fn create_table_header_cell_chunks(ctx: &InlineContext<'_>, cell: &TableCell) -> Vec<TextChunk> {
    let mut chunks = Vec::new();
    ctx.render_inline_content(&cell.tokens, &mut chunks, None);
    let base = if chunks.is_empty() { vec![ctx.create_default_chunk(JsString::from(" "), None)] } else { chunks };
    let Some(heading) = ctx.get_style("markup.heading").or_else(|| ctx.get_style("default")) else { return base };
    let attributes = create_text_attributes(heading.bold, heading.italic, heading.underline, heading.dim);
    base.into_iter()
        .map(|c| TextChunk { fg: heading.fg.or(c.fg), bg: heading.bg.or(c.bg), attributes: Some(attributes), ..c })
        .collect()
}

/// `buildTableContentCache(table, previous, forceRegenerate)`: the cache (None when the table has no columns or
/// no rows) and whether the content changed.
pub fn build_table_content_cache(
    ctx: &InlineContext<'_>,
    table: &Token,
    previous: Option<&TableContentCache>,
    force: bool,
) -> (Option<TableContentCache>, bool) {
    debug_assert_eq!(table.kind, TokenType::Table);
    let header = table.header.as_deref().unwrap_or(&[]);
    let rows = table.rows.as_deref().unwrap_or(&[]);
    let col_count = header.len();
    if col_count == 0 || rows.is_empty() {
        return (None, previous.is_some());
    }
    let mut content: TextTableContent = Vec::new();
    let mut cell_keys = Vec::new();
    let total_rows = rows.len() + 1;
    let mut changed = force || previous.is_none();
    for row_index in 0..total_rows {
        let mut row_content = Vec::new();
        let mut row_keys = vec![0u32; col_count];
        for col_index in 0..col_count {
            let is_header = row_index == 0;
            let cell = if is_header { header.get(col_index) } else { rows.get(row_index - 1).and_then(|r| r.get(col_index)) };
            let key = get_table_cell_key(cell, is_header);
            row_keys[col_index] = key;
            let previous_key = previous.and_then(|p| p.cell_keys.get(row_index)).and_then(|r| r.get(col_index)).copied();
            let previous_content = previous.and_then(|p| p.content.get(row_index)).and_then(|r| r.get(col_index));
            if !force && previous_key == Some(key) {
                if let Some(Some(chunks)) = previous_content {
                    row_content.push(Some(chunks.clone()));
                    continue;
                }
            }
            changed = true;
            row_content.push(Some(if is_header {
                create_table_header_cell_chunks(ctx, &header[col_index])
            } else {
                create_table_data_cell_chunks(ctx, cell)
            }));
        }
        content.push(row_content);
        cell_keys.push(row_keys);
    }
    if let (Some(previous), false) = (previous, changed) {
        if previous.content.len() != content.len()
            || content.iter().enumerate().any(|(i, row)| previous.content.get(i).map_or(0, |r| r.len()) != row.len())
        {
            changed = true;
        }
    }
    (Some(TableContentCache { content, cell_keys }), changed)
}
