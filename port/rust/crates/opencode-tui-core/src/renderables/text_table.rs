//! `<text_table>`: `TextTable.ts` (TextTableRenderable) drawing on `opencode-tui-markup`'s table layout
//! ([`opencode_tui_markup::table`]): a text buffer and view per cell, the measure function, the grid of borders
//! (`buffer.zig`'s `drawGrid`) and the cells.
//!
//! Divergences: selection across cells is TODO(port-tui) with the renderer's selection.
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_markup::markdown::table::TextTableContent;
use opencode_tui_markup::markdown::{BorderStyle as MdBorderStyle, ColumnFitter, ResolvedTableOptions, WrapMode as MdWrap};
use opencode_tui_markup::style::TextChunk;
use opencode_tui_markup::table::{Measure, Table, TableLayout, TableOptions};
use opencode_tui_text::{SceneText, StyledChunk, WidthMethod, WrapMode};

use crate::border::{BorderStyle, border_char_index as bci};
use crate::buffer::OptimizedBuffer;
use crate::color::{RGBA, parse_color};
use crate::layout::MeasureMode;
use crate::renderables::text::{default_options, text_context};
use crate::tree::{self, Element, Kind};

/// One cell's text buffer and view.
struct Cell {
    scene: SceneText,
}

/// The table's state, shared with its measure function.
struct State {
    content: TextTableContent,
    cells: Vec<Vec<Cell>>,
    rows: usize,
    columns: usize,
    options: TableOptions,
    show_borders: bool,
    has_explicit_outer_border: bool,
    border_style: BorderStyle,
    border_color: RGBA,
    border_background: RGBA,
    background: RGBA,
    fg: RGBA,
    bg: RGBA,
    attributes: u32,
    layout: TableLayout,
    layout_dirty: bool,
    cached_measure: Option<(Option<f64>, TableLayout)>,
}

impl State {
    fn table(&self) -> Table {
        Table { rows: self.rows, columns: self.columns, options: self.options }
    }

    fn compute_layout(&mut self, max: Option<f64>) -> TableLayout {
        let table = self.table();
        let cells = &mut self.cells;
        let mut measure = |row: usize, col: usize, width: u32| -> Option<Measure> {
            let cell = cells.get_mut(row)?.get_mut(col)?;
            let result = cell.scene.measure_for_dimensions(width);
            Some(Measure { line_count: result.line_count, width_cols_max: result.width_cols_max })
        };
        table.compute_layout(max, &mut measure)
    }

    fn wrap(&self) -> WrapMode {
        match self.options.wrap_mode {
            MdWrap::None => WrapMode::None,
            MdWrap::Char => WrapMode::Char,
            MdWrap::Word => WrapMode::Word,
        }
    }

    fn cell_options(&self) -> opencode_tui_text::SceneTextOptions {
        let mut options = default_options();
        options.fg = self.fg.0;
        options.bg = self.bg.0;
        options.attributes = self.attributes;
        options.wrap_mode = self.wrap();
        options
    }

    /// `rebuildCells(content)`: cells whose content changed get new buffers.
    fn rebuild_cells(&mut self, content: TextTableContent) {
        let rows = content.len();
        let columns = content.iter().map(|r| r.len()).max().unwrap_or(0);
        let options = self.cell_options();
        let mut cells = Vec::with_capacity(rows);
        let mut previous = std::mem::take(&mut self.cells);
        for (r, row) in content.iter().enumerate() {
            let mut out = Vec::with_capacity(columns);
            for c in 0..columns {
                let chunks: Option<&Vec<TextChunk>> = row.get(c).and_then(|cell| cell.as_ref());
                let old = self.content.get(r).and_then(|row| row.get(c)).and_then(|c| c.as_ref());
                let reuse = previous.get_mut(r).and_then(|row| row.get_mut(c)).filter(|_| old == chunks);
                if let Some(cell) = reuse {
                    let scene = std::mem::replace(&mut cell.scene, SceneText::new(WidthMethod::Unicode, &text_context()));
                    out.push(Cell { scene });
                    continue;
                }
                let mut scene = SceneText::new(WidthMethod::Unicode, &text_context());
                let _ = scene.set_options(options, || [None, None]);
                let owned: Vec<(String, Option<String>, &TextChunk)> = chunks
                    .map(|chunks| chunks.iter().map(|c| (c.text.to_string(), c.link.as_ref().map(|l| l.to_string()), c)).collect())
                    .unwrap_or_default();
                let styled: Vec<StyledChunk> = owned
                    .iter()
                    .map(|(text, link, c)| StyledChunk {
                        text,
                        fg: c.fg.map(|c| c.0),
                        bg: c.bg.map(|c| c.0),
                        attributes: c.attributes.unwrap_or(0),
                        link: link.as_deref(),
                    })
                    .collect();
                let _ = scene.set_styled_text(&styled);
                out.push(Cell { scene });
            }
            cells.push(out);
        }
        self.content = content;
        self.cells = cells;
        self.rows = rows;
        self.columns = columns;
    }

    fn invalidate(&mut self) {
        self.layout_dirty = true;
        self.cached_measure = None;
    }

    /// `rebuildLayoutForCurrentWidth()` / `applyLayoutToViews(layout)`.
    fn ensure_layout(&mut self, width: f32) {
        if !self.layout_dirty {
            return;
        }
        let max = self.table().resolve_layout_width_constraint(Some(width as f64));
        let layout = match self.cached_measure.take() {
            Some((w, layout)) if w == max => layout,
            _ => self.compute_layout(max),
        };
        let hp = self.options.cell_padding_x * 2;
        let vp = self.options.cell_padding_y * 2;
        for (r, row) in self.cells.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                let w = layout.column_widths.get(c).copied().unwrap_or(1);
                let h = layout.row_heights.get(r).copied().unwrap_or(1);
                let width = (w - hp as i64).max(1) as u32;
                let height = (h - vp as i64).max(1) as u32;
                cell.scene.apply_layout(width, height);
            }
        }
        self.layout = layout;
        self.layout_dirty = false;
    }
}

/// `TextTableRenderable`.
pub struct TextTableKind {
    state: Rc<RefCell<State>>,
}

impl Kind for TextTableKind {
    fn name(&self) -> &'static str {
        "text-table"
    }
    fn on_resize(&mut self, el: Element, _width: f32, _height: f32) {
        self.state.borrow_mut().invalidate();
        el.request_render();
    }
    fn render(&mut self, el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        let mut state = self.state.borrow_mut();
        state.ensure_layout(el.width());
        buffer.push_scissor_rect(x, y, width as i32, height as i32);
        if state.background.a() > 0.0 {
            buffer.fill_rect(x, y, width as i32, height as i32, state.background);
        }
        draw_borders(&state, buffer, x, y);
        let cell_x = x + 1 + state.options.cell_padding_x as i32;
        let padding_y = state.options.cell_padding_y as i32;
        let columns = state.layout.column_offsets.clone();
        let rows = state.layout.row_offsets.clone();
        for (r, row) in state.cells.iter_mut().enumerate() {
            let cell_y = y + rows.get(r).copied().unwrap_or(0) as i32 + 1 + padding_y;
            for (c, cell) in row.iter_mut().enumerate() {
                let cx = cell_x + columns.get(c).copied().unwrap_or(0) as i32;
                cell.scene.draw(cell_y, |src| buffer.draw_text_buffer(src, cx, cell_y));
            }
        }
        buffer.pop_scissor_rect();
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn draw_borders(state: &State, buffer: &mut OptimizedBuffer, x: i32, y: i32) {
    if !state.show_borders {
        return;
    }
    let table = state.table();
    let layout = table.border_layout();
    if table.vertical_border_count(&layout) == 0 && table.horizontal_border_count(&layout) == 0 {
        return;
    }
    let columns: Vec<i32> = state.layout.column_offsets.iter().map(|o| *o as i32 + x).collect();
    let rows: Vec<i32> = state.layout.row_offsets.iter().map(|o| *o as i32 + y).collect();
    draw_grid(
        buffer,
        &state.border_style.char_array(),
        state.border_color,
        state.border_background,
        &columns,
        &rows,
        state.options.border,
        state.options.outer_border,
    );
}

/// `tableBorderIntersectionByConnections`.
fn intersection(chars: &[u32; 11], up: bool, down: bool, left: bool, right: bool) -> u32 {
    match (up, down, left, right) {
        (true, true, true, true) => chars[bci::CROSS],
        (false, true, false, true) => chars[bci::TOP_LEFT],
        (false, true, true, false) => chars[bci::TOP_RIGHT],
        (true, false, false, true) => chars[bci::BOTTOM_LEFT],
        (true, false, true, false) => chars[bci::BOTTOM_RIGHT],
        (true, true, false, true) => chars[bci::LEFT_T],
        (true, true, true, false) => chars[bci::RIGHT_T],
        (false, true, true, true) => chars[bci::TOP_T],
        (true, false, true, true) => chars[bci::BOTTOM_T],
        _ if (left || right) && !up && !down => chars[bci::HORIZONTAL],
        _ if (up || down) && !left && !right => chars[bci::VERTICAL],
        _ => chars[bci::CROSS],
    }
}

fn set_grid_cell(buffer: &mut OptimizedBuffer, x: i32, y: i32, char: u32, fg: RGBA, bg: RGBA) {
    if let Some(ch) = char::from_u32(char) {
        buffer.set_cell_with_alpha_blending(x, y, ch, fg, bg, 0);
    }
}

/// `drawGrid` (buffer.zig): borders of the cells between `columns` and `rows` offsets (one trailing entry each).
#[allow(clippy::too_many_arguments)]
fn draw_grid(buffer: &mut OptimizedBuffer, chars: &[u32; 11], fg: RGBA, bg: RGBA, columns: &[i32], rows: &[i32], inner: bool, outer: bool) {
    let column_count = columns.len().saturating_sub(1);
    let row_count = rows.len().saturating_sub(1);
    if row_count == 0 || column_count == 0 || (!inner && !outer) {
        return;
    }
    if fg.a() == 0.0 && bg.a() == 0.0 {
        return;
    }
    let h = chars[bci::HORIZONTAL];
    let v = chars[bci::VERTICAL];
    let width = buffer.width() as i32;
    let height = buffer.height() as i32;
    let mut first_visible = 0;
    while first_visible <= column_count && columns[first_visible] < 0 {
        first_visible += 1;
    }
    for row in 0..=row_count {
        let outer_row = row == 0 || row == row_count;
        let draw_horizontal = if outer_row { outer } else { inner };
        let border_y = rows[row];
        if border_y >= height {
            break;
        }
        if draw_horizontal && border_y >= 0 {
            for col in first_visible..=column_count {
                let outer_col = col == 0 || col == column_count;
                let draw_vertical = if outer_col { outer } else { inner };
                if !draw_vertical {
                    continue;
                }
                let bx = columns[col];
                if bx >= width {
                    break;
                }
                if bx < 0 {
                    continue;
                }
                let up = row > 0;
                let down = row < row_count;
                let left = col > 0;
                let right = col < column_count;
                set_grid_cell(buffer, bx, border_y, intersection(chars, up, down, left, right), fg, bg);
            }
            let mut col = first_visible.saturating_sub(1);
            while col < column_count {
                let after = if col < column_count - 1 { inner } else { outer };
                let padding: i64 = if after { 0 } else { 1 };
                let start = columns[col] as i64 + 1;
                let end = columns[col + 1] as i64 + padding;
                if start >= width as i64 {
                    break;
                }
                if end > 0 {
                    let from = start.max(0) as i32;
                    let to = end.min(width as i64) as i32;
                    for x in from..to {
                        set_grid_cell(buffer, x, border_y, h, fg, bg);
                    }
                }
                col += 1;
            }
        }
        if row >= row_count {
            break;
        }
        let after = if row < row_count - 1 { inner } else { outer };
        let padding: i64 = if after { 0 } else { 1 };
        let start_y = (border_y as i64 + 1).max(0);
        let end_y = rows[row + 1] as i64 + padding;
        let mut cy = start_y;
        while cy < end_y && cy < height as i64 {
            for col in first_visible..=column_count {
                let outer_col = col == 0 || col == column_count;
                let draw_vertical = if outer_col { outer } else { inner };
                if !draw_vertical {
                    continue;
                }
                let bx = columns[col];
                if bx >= width {
                    break;
                }
                if bx < 0 {
                    continue;
                }
                set_grid_cell(buffer, bx, cy as i32, v, fg, bg);
            }
            cy += 1;
        }
    }
}

/// `new TextTableRenderable(ctx, { id })`: flexShrink 0, the default options, no content.
pub fn create_text_table() -> Element {
    let state = Rc::new(RefCell::new(State {
        content: Vec::new(),
        cells: Vec::new(),
        rows: 0,
        columns: 0,
        options: TableOptions::default(),
        show_borders: true,
        has_explicit_outer_border: false,
        border_style: BorderStyle::Single,
        border_color: parse_color("#FFFFFF"),
        border_background: parse_color("transparent"),
        background: parse_color("transparent"),
        fg: parse_color("#FFFFFF"),
        bg: parse_color("transparent"),
        attributes: 0,
        layout: TableLayout::empty(),
        layout_dirty: true,
        cached_measure: None,
    }));
    let measured = state.clone();
    let el = tree::create(Box::new(TextTableKind { state }), "text-table", false);
    el.set_flex_shrink(Some(0.0));
    el.set_selectable(true);
    let measure_el = el;
    el.set_measure_func(Some(Box::new(move |width, width_mode, _height, _height_mode| {
        let mut state = measured.borrow_mut();
        let defined = width_mode != MeasureMode::Undefined && width.is_finite();
        let raw = if defined { Some((width as f64).floor().max(1.0)) } else { None };
        let constraint = state.table().resolve_layout_width_constraint(raw);
        let layout = state.compute_layout(constraint);
        let mut w = if layout.table_width > 0 { layout.table_width as f64 } else { 1.0 };
        let h = if layout.table_height > 0 { layout.table_height as f64 } else { 1.0 };
        let absolute = measure_el.with_base(|b| b.position_absolute).unwrap_or(false);
        if width_mode == MeasureMode::AtMost && !absolute {
            if let Some(raw) = raw {
                w = raw.min(w);
            }
        }
        state.cached_measure = Some((constraint, layout));
        (w as f32, h as f32)
    })));
    el
}

fn border_style(style: MdBorderStyle) -> BorderStyle {
    match style {
        MdBorderStyle::Single => BorderStyle::Single,
        MdBorderStyle::Double => BorderStyle::Double,
        MdBorderStyle::Rounded => BorderStyle::Rounded,
        MdBorderStyle::Heavy => BorderStyle::Heavy,
    }
}

impl Element {
    fn with_table(self, f: impl FnOnce(&mut State) -> bool) {
        let remeasure = self.with_kind::<TextTableKind, _>(|k| f(&mut k.state.borrow_mut())).unwrap_or(false);
        if remeasure {
            self.mark_layout_dirty();
        } else {
            self.request_render();
        }
    }

    /// `content = value` on a `<text_table>`.
    pub fn set_table_content(self, content: TextTableContent) {
        self.with_table(|s| {
            s.rebuild_cells(content);
            s.invalidate();
            true
        });
    }

    /// The props a markdown table sets (`applyTableRenderableOptions`).
    pub fn set_table_options(self, o: ResolvedTableOptions) {
        self.with_table(|s| {
            let mut layout = false;
            let wrap = o.wrap_mode;
            if s.options.wrap_mode != wrap {
                s.options.wrap_mode = wrap;
                let mode = s.wrap();
                let options = s.cell_options();
                for cell in s.cells.iter_mut().flatten() {
                    let mut cell_options = options;
                    cell_options.wrap_mode = mode;
                    let _ = cell.scene.set_options(cell_options, || [None, None]);
                }
                layout = true;
            }
            let resolve = |v: f64| if v.is_finite() { v.floor().max(0.0) as u32 } else { 0 };
            let checks: [(&mut u32, u32); 3] = [
                (&mut s.options.cell_padding_x, resolve(o.cell_padding_x)),
                (&mut s.options.cell_padding_y, resolve(o.cell_padding_y)),
                (&mut s.options.column_gap, resolve(o.column_gap)),
            ];
            for (field, value) in checks {
                if *field != value {
                    *field = value;
                    layout = true;
                }
            }
            if s.options.column_width_mode != o.column_width_mode {
                s.options.column_width_mode = o.column_width_mode;
                layout = true;
            }
            let fitter = if o.column_fitter == ColumnFitter::Balanced { ColumnFitter::Balanced } else { ColumnFitter::Proportional };
            if s.options.column_fitter != fitter {
                s.options.column_fitter = fitter;
                layout = true;
            }
            if s.options.border != o.border {
                s.options.border = o.border;
                if !s.has_explicit_outer_border {
                    s.options.outer_border = o.border;
                }
                layout = true;
            }
            if s.options.outer_border != o.outer_border || !s.has_explicit_outer_border {
                s.has_explicit_outer_border = true;
                if s.options.outer_border != o.outer_border {
                    s.options.outer_border = o.outer_border;
                    layout = true;
                }
            }
            s.show_borders = o.show_borders;
            s.border_style = border_style(o.border_style);
            s.border_color = RGBA(o.border_color.0);
            let fg = o.fg.map(|c| RGBA(c.0)).unwrap_or(parse_color("#FFFFFF"));
            if s.fg != fg {
                s.fg = fg;
                let options = s.cell_options();
                for cell in s.cells.iter_mut().flatten() {
                    let _ = cell.scene.set_options(options, || [None, None]);
                }
            }
            if layout {
                s.invalidate();
            }
            layout
        });
        self.set_selectable(o.selectable);
    }
}
