//! The text buffer API the renderables call: `TextBuffer` (`text-buffer.ts`) and `TextBufferView`
//! (`text-buffer-view.ts`), with the checks and steps of the native Context calls behind them (`context.zig`:
//! `textBufferSetText`, `textBufferAppend`, `textViewSelect`, ...). A [`TextContext`] stands for the native Context:
//! it owns the link and grapheme pools that buffers and cell buffers share.
//!
//! A view keeps its buffer alive; dropping the last handle frees it (TS `destroy()`).

use crate::grapheme::GraphemePool;
use crate::link::{self, LinkPool, LinkTracker};
use crate::segment::{Highlight, TextChunk};
use crate::syntax_style::{StyleDefinition, SyntaxStyle};
use crate::text_buffer::{OwnedStyledChunk, TextBufferCore, RUN_STYLE_FLAG};
use crate::text_buffer_view::{
    LineInfo, MeasureResult, SelectionBehavior, SelectionOccupancy, TextAlign, TextBufferViewCore, TextSelection,
    VirtualChunk, VirtualLine, VirtualLineSpanInfo, Viewport, WrapMode,
};
use crate::utf8::{ControlDisplay, RenderClusterInfo, WidthMethod};
use crate::{Rgba, TextError};
use std::cell::{RefCell, RefMut};
use std::rc::Rc;

/// The shared pools of a native Context (`Context.links`, `Context.graphemes`).
#[derive(Clone, Default)]
pub struct TextContext {
    pub links: Rc<RefCell<LinkPool>>,
    pub graphemes: Rc<RefCell<GraphemePool>>,
}

impl TextContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// `getLinkUrl`.
    pub fn link_url(&self, id: u32) -> Option<String> {
        self.links.borrow().get(id).ok().map(str::to_owned)
    }
}

/// Text as TS reads it: `TextDecoder.decode`, which drops a leading byte order mark.
// TODO(port): a text that starts with U+FEFF loses it on every read in TS (getPlainText, getTextRange,
// getSelectedText); matched until the steward decides on a deviation.
pub(crate) fn decode_text(bytes: Vec<u8>) -> String {
    let text = String::from_utf8(bytes).expect("UTF-8 text");
    match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    }
}

/// `validateTextBytes`: rope columns and line counts are u32.
pub(crate) fn validate_text_bytes(tab_width: u8, bytes: usize, retained: u32) -> Result<(), TextError> {
    let max = (u32::MAX - 1) / tab_width.max(2) as u32;
    if retained > max || bytes > (max - retained) as usize {
        return Err(TextError::TextLimit);
    }
    Ok(())
}

pub(crate) fn validate_viewport(vp: Viewport) -> Result<(), TextError> {
    if vp.x as u64 + vp.width as u64 > i32::MAX as u64 || vp.y as u64 + vp.height as u64 > i32::MAX as u64 {
        return Err(TextError::InvalidOptions);
    }
    Ok(())
}

pub(crate) fn validate_tab_indicator(indicator: Option<u32>) -> Result<(), TextError> {
    if let Some(v) = indicator
        && (v < 0x20 || (0x7f..=0x9f).contains(&v) || v > 0x10ffff || (0xd800..=0xdfff).contains(&v))
    {
        return Err(TextError::InvalidOptions);
    }
    Ok(())
}

/// A highlight as TS passes it (`Highlight` in `types.ts`); out-of-range fields are rejected as the ABI does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HighlightRange {
    pub start: u32,
    pub end: u32,
    pub style_id: u32,
    pub priority: u32,
    pub hl_ref: u32,
}

impl HighlightRange {
    pub fn new(start: u32, end: u32, style_id: u32) -> Self {
        HighlightRange { start, end, style_id, priority: 0, hl_ref: 0 }
    }

    /// `highlightFromC`.
    pub(crate) fn checked(&self) -> Result<(u8, u16), TextError> {
        if self.priority > 255 || self.hl_ref > 65535 || self.style_id & RUN_STYLE_FLAG != 0 {
            return Err(TextError::InvalidOptions);
        }
        Ok((self.priority as u8, self.hl_ref as u16))
    }
}

impl From<&Highlight> for HighlightRange {
    fn from(h: &Highlight) -> Self {
        HighlightRange { start: h.col_start, end: h.col_end, style_id: h.style_id, priority: h.priority as u32, hl_ref: h.hl_ref as u32 }
    }
}

/// One chunk of styled text (`TextChunk` in `text-buffer.ts`).
#[derive(Clone, Copy, Debug)]
pub struct StyledChunk<'a> {
    pub text: &'a str,
    pub fg: Option<Rgba>,
    pub bg: Option<Rgba>,
    pub attributes: u32,
    pub link: Option<&'a str>,
}

impl<'a> StyledChunk<'a> {
    pub fn plain(text: &'a str) -> Self {
        StyledChunk { text, fg: None, bg: None, attributes: 0, link: None }
    }
}

/// Which default style fields a call sets (`NativeEditorStyleMask`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DefaultsMask {
    pub fg: bool,
    pub bg: bool,
    pub attributes: bool,
}

pub(crate) fn set_buffer_defaults(
    tb: &mut TextBufferCore,
    mask: DefaultsMask,
    fg: Option<Rgba>,
    bg: Option<Rgba>,
    attributes: Option<u32>,
) -> Result<(), TextError> {
    if !mask.fg && !mask.bg && !mask.attributes {
        return Err(TextError::InvalidOptions);
    }
    if attributes.is_some_and(|a| a & !link::attributes::BASE_MASK != 0) {
        return Err(TextError::InvalidOptions);
    }
    if mask.fg {
        tb.default_fg = fg;
    }
    if mask.bg {
        tb.default_bg = bg;
    }
    if mask.attributes {
        tb.default_attributes = attributes;
    }
    Ok(())
}

pub(crate) enum HighlightOp {
    AddLine(u32, HighlightRange),
    AddRange(HighlightRange),
    RemoveRef(u32),
    ClearLine(u32),
    ClearAll,
}

/// `highlightBuffer`.
pub(crate) fn highlight_buffer(tb: &mut TextBufferCore, op: HighlightOp) -> Result<(), TextError> {
    match op {
        HighlightOp::AddLine(line, h) => {
            let (priority, hl_ref) = h.checked()?;
            tb.add_highlight(line as usize, h.start, h.end, h.style_id, priority, hl_ref)
        }
        HighlightOp::AddRange(h) => {
            let (priority, hl_ref) = h.checked()?;
            tb.add_highlight_by_char_range(h.start, h.end, h.style_id, priority, hl_ref)
        }
        HighlightOp::RemoveRef(r) => {
            if r > 65535 {
                return Err(TextError::InvalidOptions);
            }
            tb.remove_highlights_by_ref(r as u16);
            Ok(())
        }
        HighlightOp::ClearLine(line) => {
            tb.clear_line_highlights(line as usize);
            Ok(())
        }
        HighlightOp::ClearAll => {
            tb.clear_all_highlights();
            Ok(())
        }
    }
}

/// `prepareStyledTextResource`: the byte copy, run chunks and links of styled text.
pub(crate) fn prepare_styled(
    tb: &TextBufferCore,
    chunks: &[StyledChunk],
    allow_empty_chunks: bool,
    allow_links: bool,
) -> Result<(Vec<u8>, Vec<OwnedStyledChunk>, Option<LinkTracker>), TextError> {
    let mut total = 0usize;
    for chunk in chunks {
        if (chunk.text.is_empty() && !allow_empty_chunks) || chunk.attributes & !link::attributes::BASE_MASK != 0 {
            return Err(TextError::InvalidOptions);
        }
        if chunk.link.is_some() && !allow_links {
            return Err(TextError::InvalidOptions);
        }
        total += chunk.text.len();
    }
    validate_text_bytes(tb.tab_width, total, 0)?;
    let mut bytes = Vec::with_capacity(total);
    let mut ranges = Vec::with_capacity(chunks.len());
    let mut tracker: Option<LinkTracker> = None;
    let pool = tb.link_pool().clone();
    for chunk in chunks {
        if chunk.text.is_empty() {
            continue;
        }
        let mut attributes = chunk.attributes;
        if let Some(url) = chunk.link
            && !url.is_empty()
            && url.len() <= link::MAX_URL_LENGTH
            && tb.measure_text(chunk.text.as_bytes()) > 0
        {
            let tracker = tracker.get_or_insert_with(LinkTracker::new);
            let id = tracker.track_url(&mut pool.borrow_mut(), url).map_err(|_| TextError::OutOfMemory)?;
            attributes = link::attributes::set_link_id(attributes, id);
        }
        bytes.extend_from_slice(chunk.text.as_bytes());
        ranges.push(OwnedStyledChunk {
            byte_count: chunk.text.len() as u32,
            style: StyleDefinition { fg: chunk.fg, bg: chunk.bg, attributes },
        });
    }
    Ok((bytes, ranges, tracker))
}

pub(crate) struct TextShared {
    pub(crate) core: TextBufferCore,
    input_mem_id: u8,
    views: Vec<Option<Box<TextBufferViewCore>>>,
    style: Option<SyntaxStyle>,
}

impl TextShared {
    /// `holdsBytes`: the rope holds exactly `bytes` in the input slot.
    fn holds_bytes(&self, bytes: &[u8]) -> bool {
        let registry = &self.core.mem_registry;
        registry.len() == 1
            && self.core.byte_size() as usize == bytes.len()
            && registry.get(self.input_mem_id).is_some_and(|current| current == bytes)
    }

    /// `reclaimAppendBuffers`: shared text keeps slot zero for replacements.
    fn reclaim_append_buffers(&mut self) {
        self.core.mem_registry.truncate_to_first();
    }

    fn view(&mut self, index: usize) -> (&mut TextBufferCore, &mut TextBufferViewCore) {
        let view = self.views[index].as_deref_mut().expect("live view");
        (&mut self.core, view)
    }
}

/// `TextBuffer` (`text-buffer.ts`). Clones are handles to the same buffer.
#[derive(Clone)]
pub struct TextBuffer {
    pub(crate) shared: Rc<RefCell<TextShared>>,
}

impl TextBuffer {
    /// `TextBuffer.create(widthMethod, owner)`.
    pub fn create(width_method: WidthMethod, ctx: &TextContext) -> TextBuffer {
        let mut core = TextBufferCore::new(width_method, ControlDisplay::Hidden, ctx.links.clone());
        let input_mem_id = core.mem_registry.register(Vec::new()).expect("an empty registry");
        TextBuffer { shared: Rc::new(RefCell::new(TextShared { core, input_mem_id, views: Vec::new(), style: None })) }
    }

    pub fn ptr_eq(&self, other: &TextBuffer) -> bool {
        Rc::ptr_eq(&self.shared, &other.shared)
    }

    fn borrow(&self) -> RefMut<'_, TextShared> {
        self.shared.borrow_mut()
    }

    /// `setText`. Returns whether the bytes changed (equal bytes still rebuild the rope).
    pub fn set_text(&self, text: &str) -> Result<bool, TextError> {
        let mut s = self.borrow();
        validate_text_bytes(s.core.tab_width, text.len(), 0)?;
        let changed = !s.holds_bytes(text.as_bytes());
        let id = s.input_mem_id;
        s.input_mem_id = s.core.replace_text(text.as_bytes().to_vec(), Some(id))?;
        s.reclaim_append_buffers();
        Ok(changed)
    }

    /// `append`.
    pub fn append(&self, text: &str) -> Result<(), TextError> {
        let mut s = self.borrow();
        let retained = s.core.byte_size();
        validate_text_bytes(s.core.tab_width, text.len(), retained)?;
        if text.is_empty() {
            return Ok(());
        }
        if s.core.mem_registry.free_slots() == 0 {
            s.core.flatten_mem_registry();
        }
        s.core.append_copy(text.as_bytes())
    }

    /// `setStyledText`: an empty chunk list clears the text instead (`clear()`, which keeps highlights).
    pub fn set_styled_text(&self, chunks: &[StyledChunk]) -> Result<bool, TextError> {
        if chunks.is_empty() {
            self.clear();
            return Ok(true);
        }
        let mut s = self.borrow();
        let (bytes, ranges, links) = prepare_styled(&s.core, chunks, true, true)?;
        let changed = !s.holds_bytes(&bytes);
        let id = s.input_mem_id;
        s.input_mem_id = s.core.replace_owned_styled_text(bytes, Some(id), &ranges, links)?;
        s.reclaim_append_buffers();
        Ok(changed)
    }

    /// `_setEncodedStyledText`: as `set_styled_text`, but an empty list sets an empty text (no `clear`).
    pub fn set_encoded_styled_text(&self, chunks: &[StyledChunk]) -> Result<bool, TextError> {
        let mut s = self.borrow();
        let (bytes, ranges, links) = prepare_styled(&s.core, chunks, true, true)?;
        let changed = !s.holds_bytes(&bytes);
        let id = s.input_mem_id;
        s.input_mem_id = s.core.replace_owned_styled_text(bytes, Some(id), &ranges, links)?;
        s.reclaim_append_buffers();
        Ok(changed)
    }

    /// `TextBuffer._replaceStyledTextBatch`: replaces several buffers' styled text in one step; each view's local
    /// selection is reset. `None` (nothing changed) when the batch is over the native limits (256 replacements,
    /// 4096 chunks, 4 MiB of text, 1 MiB of URLs). Returns each buffer's `(length, byteSize)`.
    pub fn replace_styled_text_batch(
        replacements: &[(&TextBuffer, &TextBufferView, &[StyledChunk])],
    ) -> Result<Option<Vec<(u32, u32)>>, TextError> {
        let (mut bytes, mut chunks, mut urls) = (0usize, 0usize, 0usize);
        for (_, _, c) in replacements {
            chunks += c.len();
            bytes += c.iter().map(|c| c.text.len()).sum::<usize>();
            urls += c.iter().filter_map(|c| c.link).map(str::len).sum::<usize>();
        }
        if replacements.len() > 256 || bytes > 4 * 1024 * 1024 || chunks > 4096 || urls > 1024 * 1024 {
            return Ok(None);
        }
        for (i, (buffer, view, _)) in replacements.iter().enumerate() {
            if !Rc::ptr_eq(&buffer.shared, &view.shared) || replacements[..i].iter().any(|(b, _, _)| b.ptr_eq(buffer)) {
                return Err(TextError::InvalidOptions);
            }
        }
        let mut prepared = Vec::with_capacity(replacements.len());
        for (buffer, _, c) in replacements {
            prepared.push(prepare_styled(&buffer.borrow().core, c, true, true)?);
        }
        let mut out = Vec::with_capacity(replacements.len());
        for ((buffer, view, _), (bytes, ranges, links)) in replacements.iter().zip(prepared) {
            let mut s = buffer.borrow();
            let id = s.input_mem_id;
            s.input_mem_id = s.core.replace_owned_styled_text(bytes, Some(id), &ranges, links)?;
            s.reclaim_append_buffers();
            let (tb, v) = s.view(view.index);
            v.reset_local_selection();
            out.push((tb.length(), tb.byte_size()));
        }
        Ok(Some(out))
    }

    fn set_defaults(&self, mask: DefaultsMask, fg: Option<Rgba>, bg: Option<Rgba>, attributes: Option<u32>) -> Result<(), TextError> {
        set_buffer_defaults(&mut self.borrow().core, mask, fg, bg, attributes)
    }

    /// `_setDefaults`: all three at once.
    pub fn set_all_defaults(&self, fg: Option<Rgba>, bg: Option<Rgba>, attributes: Option<u32>) -> Result<(), TextError> {
        self.set_defaults(DefaultsMask { fg: true, bg: true, attributes: true }, fg, bg, attributes)
    }

    pub fn set_default_fg(&self, fg: Option<Rgba>) -> Result<(), TextError> {
        self.set_defaults(DefaultsMask { fg: true, bg: false, attributes: false }, fg, None, None)
    }

    pub fn set_default_bg(&self, bg: Option<Rgba>) -> Result<(), TextError> {
        self.set_defaults(DefaultsMask { fg: false, bg: true, attributes: false }, None, bg, None)
    }

    pub fn set_default_attributes(&self, attributes: Option<u32>) -> Result<(), TextError> {
        self.set_defaults(DefaultsMask { fg: false, bg: false, attributes: true }, None, None, attributes)
    }

    pub fn reset_defaults(&self) {
        let _ = self.set_all_defaults(None, None, None);
    }

    pub fn get_line_count(&self) -> u32 {
        self.borrow().core.line_count()
    }

    /// `length`: display cells, without line breaks.
    pub fn length(&self) -> u32 {
        self.borrow().core.length()
    }

    /// `byteSize`: UTF-8 bytes, with one per line break.
    pub fn byte_size(&self) -> u32 {
        self.borrow().core.byte_size()
    }

    pub fn get_plain_text(&self) -> String {
        decode_text(self.borrow().core.plain_text())
    }

    /// `getTextRange`: by offsets (cells plus one per line break), snapped to graphemes.
    pub fn get_text_range(&self, start: u32, end: u32) -> String {
        if start >= end {
            return String::new();
        }
        decode_text(self.borrow().core.copy_text_range(start, end))
    }

    fn highlight(&self, op: HighlightOp) -> Result<(), TextError> {
        highlight_buffer(&mut self.borrow().core, op)
    }

    /// `addHighlightByCharRange`: offsets in cells that exclude line breaks.
    pub fn add_highlight_by_char_range(&self, h: HighlightRange) -> Result<(), TextError> {
        self.highlight(HighlightOp::AddRange(h))
    }

    /// `addHighlight`: columns on one line.
    pub fn add_highlight(&self, line: u32, h: HighlightRange) -> Result<(), TextError> {
        self.highlight(HighlightOp::AddLine(line, h))
    }

    pub fn remove_highlights_by_ref(&self, hl_ref: u32) -> Result<(), TextError> {
        self.highlight(HighlightOp::RemoveRef(hl_ref))
    }

    pub fn clear_line_highlights(&self, line: u32) {
        let _ = self.highlight(HighlightOp::ClearLine(line));
    }

    pub fn clear_all_highlights(&self) {
        let _ = self.highlight(HighlightOp::ClearAll);
    }

    pub fn get_line_highlights(&self, line: u32) -> Vec<HighlightRange> {
        self.borrow().core.line_highlights(line as usize).iter().map(HighlightRange::from).collect()
    }

    pub fn get_highlight_count(&self) -> u32 {
        self.borrow().core.highlight_count()
    }

    pub fn set_syntax_style(&self, style: Option<&SyntaxStyle>) {
        let mut s = self.borrow();
        s.core.set_syntax_style(style.cloned());
        s.style = style.cloned();
    }

    pub fn get_syntax_style(&self) -> Option<SyntaxStyle> {
        self.borrow().style.clone()
    }

    /// `setTabWidth`: rounded up to an even width in `[2, 254]`.
    pub fn set_tab_width(&self, width: u32) -> Result<(), TextError> {
        if width > 255 {
            return Err(TextError::InvalidOptions);
        }
        let mut s = self.borrow();
        s.core.check_tab_width(width as u8)?;
        s.core.set_tab_width(width as u8);
        Ok(())
    }

    pub fn get_tab_width(&self) -> u8 {
        self.borrow().core.tab_width
    }

    /// `clear`: empties the text, keeping highlights.
    pub fn clear(&self) {
        self.borrow().core.clear();
    }

    /// `reset`: empties the text and drops highlights, styles and registered bytes.
    pub fn reset(&self) {
        let mut s = self.borrow();
        s.core.reset();
        s.input_mem_id = s.core.mem_registry.register(Vec::new()).expect("an empty registry");
    }

    pub fn width_method(&self) -> WidthMethod {
        self.borrow().core.width_method
    }

    /// The content epoch: it changes with every content change.
    pub fn content_epoch(&self) -> u64 {
        self.borrow().core.content_epoch()
    }

    /// Bytes the buffer holds (slots, highlights excluded).
    pub fn retained_bytes(&self) -> usize {
        self.borrow().core.mem_registry.retained_bytes()
    }
}

/// `TextBufferView` (`text-buffer-view.ts`).
pub struct TextBufferView {
    shared: Rc<RefCell<TextShared>>,
    index: usize,
}

impl Drop for TextBufferView {
    fn drop(&mut self) {
        if let Ok(mut s) = self.shared.try_borrow_mut() {
            s.views[self.index] = None;
        }
    }
}

/// Selection colors and coordinates of a selection call (`NativeEditorSelection`).
pub(crate) fn check_local_coords(
    view: &TextBufferViewCore,
    tb: &TextBufferCore,
    anchor_x: i32,
    anchor_y: i32,
    focus_x: i32,
    focus_y: i32,
) -> Result<(), TextError> {
    if let Some(vp) = view.viewport {
        let x = if view.wrap_mode == WrapMode::None { i32::try_from(vp.x).map_err(|_| TextError::InvalidOptions)? } else { 0 };
        let y = i32::try_from(vp.y).map_err(|_| TextError::InvalidOptions)?;
        for (value, offset) in [(anchor_x, x), (focus_x, x), (anchor_y, y), (focus_y, y)] {
            value.checked_add(offset).ok_or(TextError::InvalidOptions)?;
        }
    }
    if tb.line_width_cols_max() > i32::MAX as u32 {
        return Err(TextError::InvalidOptions);
    }
    Ok(())
}

impl TextBufferView {
    /// `TextBufferView.create(textBuffer)`.
    pub fn create(buffer: &TextBuffer) -> TextBufferView {
        let mut s = buffer.shared.borrow_mut();
        let index = match s.views.iter().position(Option::is_none) {
            Some(free) => free,
            None => {
                s.views.push(None);
                s.views.len() - 1
            }
        };
        s.views[index] = Some(Box::new(TextBufferViewCore::new()));
        TextBufferView { shared: buffer.shared.clone(), index }
    }

    pub fn buffer(&self) -> TextBuffer {
        TextBuffer { shared: self.shared.clone() }
    }

    fn with<R>(&self, f: impl FnOnce(&mut TextBufferCore, &mut TextBufferViewCore) -> R) -> R {
        let mut s = self.shared.borrow_mut();
        let (tb, view) = s.view(self.index);
        f(tb, view)
    }

    pub fn set_selection(&self, start: u32, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.with(|tb, v| {
            v.update_virtual_lines(tb);
            v.set_selection(tb, start, end, bg, fg)
        })
    }

    pub fn update_selection(&self, end: u32, bg: Option<Rgba>, fg: Option<Rgba>) {
        self.with(|tb, v| {
            v.update_virtual_lines(tb);
            v.update_selection(tb, end, bg, fg)
        })
    }

    pub fn reset_selection(&self) {
        self.with(|_, v| v.reset_selection())
    }

    /// `getSelection`: `None` for no selection or an empty one.
    pub fn get_selection(&self) -> Option<(u32, u32)> {
        self.with(|_, v| v.selection_info())
    }

    pub fn has_selection(&self) -> bool {
        self.get_selection().is_some()
    }

    /// The selection with its colors, empty ones included (for drawing).
    pub fn selection(&self) -> Option<TextSelection> {
        self.with(|_, v| v.selection)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_local_selection(
        &self,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        behavior: SelectionBehavior,
    ) -> Result<bool, TextError> {
        self.with(|tb, v| {
            check_local_coords(v, tb, anchor_x, anchor_y, focus_x, focus_y)?;
            v.update_virtual_lines(tb);
            Ok(v.set_local_selection(tb, anchor_x, anchor_y, focus_x, focus_y, bg, fg, behavior))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_local_selection(
        &self,
        anchor_x: i32,
        anchor_y: i32,
        focus_x: i32,
        focus_y: i32,
        bg: Option<Rgba>,
        fg: Option<Rgba>,
        behavior: SelectionBehavior,
    ) -> Result<bool, TextError> {
        self.with(|tb, v| {
            check_local_coords(v, tb, anchor_x, anchor_y, focus_x, focus_y)?;
            v.update_virtual_lines(tb);
            Ok(v.update_local_selection(tb, anchor_x, anchor_y, focus_x, focus_y, bg, fg, behavior))
        })
    }

    pub fn reset_local_selection(&self) {
        self.with(|_, v| v.reset_local_selection())
    }

    pub fn set_selection_occupancy(&self, occupancy: SelectionOccupancy) {
        self.with(|tb, v| {
            v.update_virtual_lines(tb);
            v.set_selection_occupancy(tb, occupancy)
        })
    }

    pub fn get_selection_occupancy(&self) -> SelectionOccupancy {
        self.with(|_, v| v.selection_occupancy)
    }

    pub fn set_wrap_width(&self, width: Option<u32>) -> Result<(), TextError> {
        if width.is_some_and(|w| w > i32::MAX as u32) {
            return Err(TextError::InvalidOptions);
        }
        self.with(|_, v| v.set_wrap_width(width.filter(|&w| w != 0)));
        Ok(())
    }

    pub fn set_wrap_mode(&self, mode: WrapMode) {
        self.with(|_, v| v.set_wrap_mode(mode))
    }

    pub fn set_text_align(&self, alignment: TextAlign) {
        self.with(|_, v| v.text_align = alignment)
    }

    pub fn set_first_line_offset(&self, offset: u32) -> Result<(), TextError> {
        if offset > i32::MAX as u32 {
            return Err(TextError::InvalidOptions);
        }
        self.with(|_, v| v.set_first_line_offset(offset));
        Ok(())
    }

    pub fn set_viewport_size(&self, width: u32, height: u32) -> Result<(), TextError> {
        self.with(|_, v| {
            validate_viewport(Viewport { x: 0, y: 0, width, height })?;
            if let Some(previous) = v.viewport {
                validate_viewport(Viewport { x: previous.x, y: previous.y, width, height })?;
            }
            v.set_viewport_size(width, height);
            Ok(())
        })
    }

    pub fn set_viewport(&self, x: u32, y: u32, width: u32, height: u32) -> Result<(), TextError> {
        let vp = Viewport { x, y, width, height };
        validate_viewport(vp)?;
        self.with(|_, v| v.set_viewport(Some(vp)));
        Ok(())
    }

    pub fn get_viewport(&self) -> Option<Viewport> {
        self.with(|_, v| v.viewport)
    }

    /// `lineInfo`: the viewport's visual lines (all of them without a viewport).
    pub fn line_info(&self) -> LineInfo {
        self.with(|tb, v| v.cached_line_info(tb))
    }

    /// `logicalLineInfo`: every visual line.
    pub fn logical_line_info(&self) -> LineInfo {
        self.with(|tb, v| v.logical_line_info(tb))
    }

    /// `getLineSources`: the source lines of `line_count` visual lines from `start_line`.
    pub fn get_line_sources(&self, start_line: u32, line_count: u32) -> Vec<u32> {
        self.with(|tb, v| v.logical_line_info_window(tb, start_line as usize, line_count as usize).line_sources)
    }

    pub fn get_selected_text(&self) -> String {
        self.with(|tb, v| decode_text(v.selected_text(tb)))
    }

    pub fn get_plain_text(&self) -> String {
        self.buffer().get_plain_text()
    }

    /// `setTabIndicator`: a code point (0 for none).
    pub fn set_tab_indicator(&self, code_point: u32) -> Result<(), TextError> {
        let indicator = (code_point != 0).then_some(code_point);
        validate_tab_indicator(indicator)?;
        self.with(|_, v| v.tab_indicator = indicator);
        Ok(())
    }

    pub fn set_tab_indicator_color(&self, color: Option<Rgba>) {
        self.with(|_, v| v.tab_indicator_color = color)
    }

    pub fn set_truncate(&self, truncate: bool) {
        self.with(|_, v| v.set_truncate(truncate))
    }

    /// `measureForDimensions`: rows and widest row of the text wrapped at `width` (0: unwrapped).
    pub fn measure_for_dimensions(&self, width: u32, height: u32) -> Result<MeasureResult, TextError> {
        if width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(TextError::InvalidOptions);
        }
        Ok(self.with(|tb, v| v.measure_for_dimensions(tb, width)))
    }

    /// The size Yoga's measure function returns for a text node (`NativeRenderable.measure`). `width_mode` and
    /// `height_mode` are Yoga's (`0` undefined, `1` exactly, `2` at most); `absolute` is the node's position type.
    pub fn yoga_measure(&self, width: f32, width_mode: u32, height: f32, absolute: bool) -> (f32, f32) {
        self.with(|tb, v| yoga_measure_with(width, width_mode, height, absolute, |w| v.measure_for_dimensions(tb, w)))
    }

    /// `getVirtualLineCount`: every visual line.
    pub fn get_virtual_line_count(&self) -> u32 {
        self.with(|tb, v| v.virtual_line_count(tb))
    }

    /// Prepares the layout and passes what drawing reads (as `buffer.zig`'s `drawTextBufferInternal` does
    /// through the view). `draw_y` is the row the view is drawn at (hit-testing and alignment read it).
    /// `drawTextViewOn` calls this only when the origin is inside the target (`x < width && y < height`).
    pub fn draw<R>(&self, draw_y: i32, f: impl FnOnce(&DrawSource) -> R) -> R {
        let mut s = self.shared.borrow_mut();
        let (tb, view) = s.view(self.index);
        view.set_draw_y(draw_y);
        view.virtual_lines(tb);
        f(&DrawSource { tb, view, default_bg_fill: None })
    }

    /// Layout bytes this view holds.
    pub fn retained_bytes(&self) -> usize {
        self.with(|_, v| v.layout_bytes())
    }
}

/// `NativeRenderable.measure`: Yoga's measure of a text node from `measure(width)`.
pub(crate) fn yoga_measure_with(
    width: f32,
    width_mode: u32,
    height: f32,
    absolute: bool,
    measure: impl FnOnce(u32) -> MeasureResult,
) -> (f32, f32) {
    let effective_width = if width_mode == 0 || width.is_nan() { 0.0 } else { width };
    let effective_height = if height.is_nan() { 1.0 } else { height };
    let floor = |value: f32| -> u32 {
        if !value.is_finite() || value <= 0.0 {
            return 0;
        }
        let floored = value.floor();
        if floored >= u32::MAX as f32 { u32::MAX } else { floored as u32 }
    };
    let result = measure(floor(effective_width));
    let mut w = result.width_cols_max.max(1) as f32;
    let mut h = result.line_count.max(1) as f32;
    if width_mode == 2 && !absolute {
        w = effective_width.min(w);
        h = effective_height.min(h);
    }
    (w, h)
}

/// What a draw reads from a prepared view and its buffer.
pub struct DrawSource<'a> {
    pub(crate) tb: &'a TextBufferCore,
    pub(crate) view: &'a TextBufferViewCore,
    /// An editor's default background, filled over the viewport before the lines (`prefilledViewportBg`).
    pub(crate) default_bg_fill: Option<Rgba>,
}

impl<'a> DrawSource<'a> {
    /// The lines in the viewport (`getVirtualLines`), truncated when enabled.
    pub fn virtual_lines(&self) -> &'a [VirtualLine] {
        let all = self.view.virtual_lines_all();
        match self.view.viewport {
            Some(vp) => {
                let start = (vp.y as usize).min(all.len());
                let end = (start + vp.height as usize).min(all.len());
                &all[start..end]
            }
            None => all,
        }
    }

    pub fn viewport(&self) -> Option<Viewport> {
        self.view.viewport
    }

    /// The start offsets of the lines in the viewport (`getCachedLineInfo().line_start_cols`).
    pub fn line_start_cols(&self) -> &'a [u32] {
        let all_starts = self.view.line_starts();
        match self.view.viewport {
            Some(vp) => {
                let start = (vp.y as usize).min(all_starts.len());
                let end = (start + vp.height as usize).min(all_starts.len());
                &all_starts[start..end]
            }
            None => all_starts,
        }
    }

    pub fn selection(&self) -> Option<TextSelection> {
        self.view.selection
    }

    /// `getLineAlignmentPad`: `vline_idx` is absolute (viewport y included).
    pub fn line_alignment_pad(&self, vline_idx: usize, line_width: u32) -> u32 {
        self.view.line_alignment_pad(vline_idx, line_width)
    }

    /// `getVirtualLineSpans`: `vline_idx` is absolute.
    pub fn virtual_line_spans(&self, vline_idx: usize) -> VirtualLineSpanInfo<'a> {
        self.view.virtual_line_spans(self.tb, vline_idx)
    }

    /// The bytes of a chunk (`chunk.getBytes`); the whole chunk, not the visual slice.
    pub fn chunk_bytes(&self, chunk: &VirtualChunk) -> &'a [u8] {
        chunk.chunk().bytes(&self.tb.mem_registry)
    }

    /// The chunk's render clusters (`getRenderClusters`): multibyte graphemes and tabs with their columns.
    pub fn with_render_clusters<R>(&self, chunk: &TextChunk, f: impl FnOnce(&[RenderClusterInfo]) -> R) -> R {
        chunk.render_clusters(&self.tb.mem_registry, self.tb.tab_width, self.tb.control_display, self.tb.width_method, f)
    }

    pub fn tab_width(&self) -> u8 {
        self.tb.tab_width
    }

    pub fn control_display(&self) -> ControlDisplay {
        self.tb.control_display
    }

    pub fn width_method(&self) -> WidthMethod {
        self.tb.width_method
    }

    /// The buffer's default fg, bg and attributes (`defaults()`).
    pub fn defaults(&self) -> (Option<Rgba>, Option<Rgba>, Option<u32>) {
        (self.tb.default_fg, self.tb.default_bg, self.tb.default_attributes)
    }

    /// `resolveStyle`: a span's style id to its definition.
    pub fn resolve_style(&self, id: u32) -> Option<StyleDefinition> {
        self.tb.resolve_style(id)
    }

    pub fn tab_indicator(&self) -> Option<u32> {
        self.view.tab_indicator
    }

    pub fn tab_indicator_color(&self) -> Option<Rgba> {
        self.view.tab_indicator_color
    }

    /// Logical lines of the buffer (`lineCount()`).
    pub fn line_count(&self) -> u32 {
        self.tb.line_count()
    }

    /// The editor's prefilled background (`EditorView` only).
    pub fn default_bg_fill(&self) -> Option<Rgba> {
        self.default_bg_fill
    }
}
