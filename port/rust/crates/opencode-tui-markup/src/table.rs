//! `TextTable.ts`'s layout (column widths, row heights, offsets, the measure function) and
//! `text-table-width.ts`. Cells are measured by the caller: TS asks each cell's native text buffer view
//! (`measureForDimensions(width, height)`, width 0 for the intrinsic size), which is the text slice's wrapping.
//!
//! Divergences: the cells' text buffers, drawing and selection stay with the renderer; [`TableLayout`] carries
//! the numbers `computeLayout` returns.

use crate::markdown::{ColumnFitter, ColumnWidthMode, WrapMode};

/// `MeasureResult`: a cell's wrapped size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measure {
    pub line_count: u32,
    pub width_cols_max: u32,
}

/// Measures cells: `measure(row, col, width)` is `cell.textBufferView.measureForDimensions(width, 10_000)`.
pub trait CellMeasurer {
    fn measure(&mut self, row: usize, col: usize, width: u32) -> Option<Measure>;
}

impl<F: FnMut(usize, usize, u32) -> Option<Measure>> CellMeasurer for F {
    fn measure(&mut self, row: usize, col: usize, width: u32) -> Option<Measure> {
        self(row, col, width)
    }
}

/// The layout props of a `TextTableRenderable` (after its resolve* clamping).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableOptions {
    pub wrap_mode: WrapMode,
    pub column_width_mode: ColumnWidthMode,
    pub column_fitter: ColumnFitter,
    pub cell_padding_x: u32,
    pub cell_padding_y: u32,
    pub column_gap: u32,
    pub border: bool,
    pub outer_border: bool,
}

impl Default for TableOptions {
    /// `_defaultOptions`.
    fn default() -> Self {
        TableOptions {
            wrap_mode: WrapMode::Word,
            column_width_mode: ColumnWidthMode::Full,
            column_fitter: ColumnFitter::Proportional,
            cell_padding_x: 0,
            cell_padding_y: 0,
            column_gap: 0,
            border: true,
            outer_border: true,
        }
    }
}

/// `resolveCellPadding` / `resolveColumnGap`: a non-negative integer, or the default for a non-finite value.
pub fn resolve_non_negative(value: Option<f64>, default: u32) -> u32 {
    match value {
        Some(v) if v.is_finite() => v.floor().max(0.0) as u32,
        _ => default,
    }
}

/// `ResolvedTableBorderLayout`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorderLayout {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
    pub inner_vertical: bool,
    pub inner_horizontal: bool,
}

/// `TextTableLayout` (without the Int32Array copies).
#[derive(Clone, Debug, PartialEq)]
pub struct TableLayout {
    pub column_widths: Vec<i64>,
    pub row_heights: Vec<i64>,
    pub column_offsets: Vec<i64>,
    pub row_offsets: Vec<i64>,
    pub table_width: i64,
    pub table_height: i64,
}

impl TableLayout {
    /// `createEmptyLayout()`.
    pub fn empty() -> TableLayout {
        TableLayout {
            column_widths: Vec::new(),
            row_heights: Vec::new(),
            column_offsets: vec![0],
            row_offsets: vec![0],
            table_width: 0,
            table_height: 0,
        }
    }
}

/// A table's dimensions and options: what the layout functions of `TextTableRenderable` read.
pub struct Table {
    pub rows: usize,
    pub columns: usize,
    pub options: TableOptions,
}

/// JS `Math.min` / `Math.floor` over numbers that may be NaN.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}

/// `comparePriority`: exact `leftGrowth² · rightCapacity` against `rightGrowth² · leftCapacity`.
fn compare_priority(lg: f64, lc: f64, rg: f64, rc: f64) -> i32 {
    if [lg, lc, rg, rc].iter().all(|v| v.is_finite() && v.fract() == 0.0) {
        let (lg, lc, rg, rc) = (lg as i128, lc as i128, rg as i128, rc as i128);
        let left = lg * lg * rc;
        let right = rg * rg * lc;
        return if left < right { -1 } else if left > right { 1 } else { 0 };
    }
    let left = lg * lg * rc;
    let right = rg * rg * lc;
    if left < right { -1 } else if left > right { 1 } else { 0 }
}

/// `allocateProportionalColumnWidths(widths, targetWidth, minWidth)`.
pub fn allocate_proportional_column_widths(widths: &[f64], target_width: f64, min_width: f64) -> Vec<f64> {
    let base: Vec<f64> = widths.iter().map(|w| min_width.max(w.floor())).collect();
    let n = base.len() as f64;
    let total_base: f64 = base.iter().sum();
    let capacity: Vec<f64> = base.iter().map(|w| w - min_width).collect();
    let mut growth = vec![0f64; base.len()];
    let available = js_min((target_width - min_width * n).max(0.0), total_base - min_width * n);
    if available == 0.0 {
        return growth.iter().map(|_| min_width).collect();
    }
    if available == capacity.iter().sum::<f64>() {
        return base;
    }
    let weights: Vec<f64> = capacity.iter().map(|c| c.sqrt()).collect();
    let mut active: Vec<(usize, f64, f64)> =
        capacity.iter().enumerate().map(|(i, &w)| (i, w, weights[i])).filter(|c| c.1 > 0.0).collect();
    active.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    if active.len() == capacity.len() && capacity.iter().all(|&w| w == capacity[0]) {
        let shared = (available / n).floor();
        let remainder = available % n;
        return growth.iter().enumerate().map(|(i, _)| min_width + shared + if (i as f64) < remainder { 1.0 } else { 0.0 }).collect();
    }
    let mut remaining = available;
    let mut total_weight = active.iter().fold(0.0, |s, c| s + c.2);
    for &(idx, width, weight) in &active {
        if remaining / total_weight <= weight {
            break;
        }
        growth[idx] = width;
        remaining -= width;
        total_weight -= weight;
    }
    let level = remaining / total_weight;
    for &(idx, width, weight) in &active {
        if growth[idx] == width {
            continue;
        }
        growth[idx] = js_min(width, (level * weight).floor());
    }
    let mut allocated: f64 = growth.iter().sum();
    while allocated > available {
        let mut worst: Option<usize> = None;
        for idx in 0..base.len() {
            if growth[idx] == 0.0 {
                continue;
            }
            let comparison = match worst {
                None => 1,
                Some(w) => compare_priority(growth[idx], capacity[idx], growth[w], capacity[w]),
            };
            if comparison > 0 || (comparison == 0 && worst.is_some_and(|w| idx > w)) {
                worst = Some(idx);
            }
        }
        let Some(w) = worst else { break };
        growth[w] -= 1.0;
        allocated -= 1.0;
    }
    while allocated < available {
        let mut best: Option<usize> = None;
        for idx in 0..base.len() {
            if growth[idx] >= capacity[idx] {
                continue;
            }
            let comparison = match best {
                None => -1,
                Some(b) => compare_priority(growth[idx] + 1.0, capacity[idx], growth[b] + 1.0, capacity[b]),
            };
            if comparison < 0 {
                best = Some(idx);
            }
        }
        let Some(b) = best else { break };
        growth[b] += 1.0;
        allocated += 1.0;
    }
    growth.iter().map(|g| g + min_width).collect()
}

impl Table {
    fn horizontal_padding(&self) -> f64 {
        (self.options.cell_padding_x * 2) as f64
    }

    fn vertical_padding(&self) -> f64 {
        (self.options.cell_padding_y * 2) as f64
    }

    fn is_full_width(&self) -> bool {
        self.options.column_width_mode == ColumnWidthMode::Full
    }

    /// `resolveBorderLayout()`.
    pub fn border_layout(&self) -> BorderLayout {
        let o = &self.options;
        BorderLayout {
            left: o.outer_border,
            right: o.outer_border,
            top: o.outer_border,
            bottom: o.outer_border,
            inner_vertical: o.border && self.columns > 1,
            inner_horizontal: o.border && self.rows > 1,
        }
    }

    /// `getVerticalBorderCount(borderLayout)`.
    pub fn vertical_border_count(&self, b: &BorderLayout) -> usize {
        b.left as usize + b.right as usize + if b.inner_vertical { self.columns.saturating_sub(1) } else { 0 }
    }

    /// `getHorizontalBorderCount(borderLayout)`.
    pub fn horizontal_border_count(&self, b: &BorderLayout) -> usize {
        b.top as usize + b.bottom as usize + if b.inner_horizontal { self.rows.saturating_sub(1) } else { 0 }
    }

    fn inter_column_gap(&self, b: &BorderLayout) -> f64 {
        if b.inner_vertical { 0.0 } else { self.options.column_gap as f64 }
    }

    fn total_inter_column_gap(&self, b: &BorderLayout) -> f64 {
        self.columns.saturating_sub(1) as f64 * self.inter_column_gap(b)
    }

    /// `resolveLayoutWidthConstraint(width)`.
    pub fn resolve_layout_width_constraint(&self, width: Option<f64>) -> Option<f64> {
        let w = width.filter(|w| w.is_finite() && *w > 0.0)?;
        if self.options.wrap_mode != WrapMode::None || self.is_full_width() {
            return Some(w.floor().max(1.0));
        }
        None
    }

    /// `computeLayout(maxTableWidth)`.
    pub fn compute_layout(&self, max_table_width: Option<f64>, cells: &mut dyn CellMeasurer) -> TableLayout {
        if self.rows == 0 || self.columns == 0 {
            return TableLayout::empty();
        }
        let borders = self.border_layout();
        let intrinsic: Vec<Vec<Option<Measure>>> =
            (0..self.rows).map(|r| (0..self.columns).map(|c| cells.measure(r, c, 0)).collect()).collect();
        let column_widths = self.compute_column_widths(max_table_width, &borders, &intrinsic);
        let row_heights = self.compute_row_heights(&column_widths, &intrinsic, cells);
        let column_offsets = compute_offsets(
            &column_widths,
            borders.left,
            borders.right,
            borders.inner_vertical,
            self.inter_column_gap(&borders),
        );
        let row_offsets = compute_offsets(&row_heights, borders.top, borders.bottom, borders.inner_horizontal, 0.0);
        let to_int = |v: &Vec<f64>| v.iter().map(|&x| x as i64).collect::<Vec<i64>>();
        TableLayout {
            table_width: *column_offsets.last().unwrap_or(&0.0) as i64 + 1,
            table_height: *row_offsets.last().unwrap_or(&0.0) as i64 + 1,
            column_widths: to_int(&column_widths),
            row_heights: to_int(&row_heights),
            column_offsets: to_int(&column_offsets),
            row_offsets: to_int(&row_offsets),
        }
    }

    fn compute_column_widths(&self, max: Option<f64>, borders: &BorderLayout, intrinsic: &[Vec<Option<Measure>>]) -> Vec<f64> {
        let hp = self.horizontal_padding();
        let mut widths = vec![1.0 + hp; self.columns];
        for row in intrinsic.iter().take(self.rows) {
            for (c, m) in row.iter().enumerate().take(self.columns) {
                let measured = (m.map_or(0, |m| m.width_cols_max) as f64).max(1.0) + hp;
                widths[c] = widths[c].max(measured);
            }
        }
        let Some(max) = max.filter(|m| m.is_finite() && *m > 0.0) else { return widths };
        let max_content = (max.floor() - self.vertical_border_count(borders) as f64 - self.total_inter_column_gap(borders)).max(1.0);
        let current: f64 = widths.iter().sum();
        if current == max_content {
            return widths;
        }
        if current < max_content {
            return if self.is_full_width() { expand_column_widths(&widths, max_content) } else { widths };
        }
        if self.options.wrap_mode == WrapMode::None {
            return widths;
        }
        match self.options.column_fitter {
            ColumnFitter::Balanced => self.fit_balanced(&widths, max_content),
            ColumnFitter::Proportional => allocate_proportional_column_widths(&widths, max_content, 1.0 + hp),
        }
    }

    fn fit_balanced(&self, widths: &[f64], target: f64) -> Vec<f64> {
        let min_width = 1.0 + self.horizontal_padding();
        let hard_min = vec![min_width; widths.len()];
        let base: Vec<f64> = widths.iter().map(|w| w.floor().max(1.0)).collect();
        let total_base: f64 = base.iter().sum();
        let columns = base.len();
        if columns == 0 || total_base <= target {
            return base;
        }
        let even = min_width.max((target / columns as f64).floor());
        let preferred: Vec<f64> = base.iter().map(|&w| w.min(even)).collect();
        let preferred_total: f64 = preferred.iter().sum();
        let floor = if preferred_total <= target { preferred } else { hard_min };
        let floor_total: f64 = floor.iter().sum();
        let clamped = floor_total.max(target);
        if total_base <= clamped {
            return base;
        }
        let shrinkable: Vec<f64> = base.iter().zip(&floor).map(|(w, f)| w - f).collect();
        let total_shrinkable: f64 = shrinkable.iter().sum();
        if total_shrinkable <= 0.0 {
            return floor;
        }
        let shrink = allocate_shrink_by_weight(&shrinkable, total_base - clamped, true);
        base.iter().enumerate().map(|(i, &w)| floor[i].max(w - shrink[i])).collect()
    }

    fn compute_row_heights(&self, column_widths: &[f64], intrinsic: &[Vec<Option<Measure>>], cells: &mut dyn CellMeasurer) -> Vec<f64> {
        let hp = self.horizontal_padding();
        let vp = self.vertical_padding();
        let mut heights = vec![1.0 + vp; self.rows];
        for r in 0..self.rows {
            for c in 0..self.columns {
                let width = (column_widths.get(c).copied().unwrap_or(1.0) - hp).max(1.0);
                let measure = match intrinsic[r][c] {
                    Some(m) if self.options.wrap_mode == WrapMode::None || (m.width_cols_max as f64) <= width => Some(m),
                    _ => cells.measure(r, c, width as u32),
                };
                let lines = (measure.map_or(1, |m| m.line_count) as f64).max(1.0);
                heights[r] = heights[r].max(lines + vp);
            }
        }
        heights
    }

    /// The measure function (`setupMeasureFunc`): the size Yoga gets for `width` under `width_mode`.
    pub fn measure(&self, width: f64, width_mode_defined: bool, at_most: bool, absolute: bool, cells: &mut dyn CellMeasurer) -> (f64, f64, TableLayout) {
        let raw = if width_mode_defined && width.is_finite() { Some(width.floor().max(1.0)) } else { None };
        let constraint = self.resolve_layout_width_constraint(raw);
        let layout = self.compute_layout(constraint, cells);
        let mut w = if layout.table_width > 0 { layout.table_width as f64 } else { 1.0 };
        let h = if layout.table_height > 0 { layout.table_height as f64 } else { 1.0 };
        if at_most && !absolute {
            if let Some(raw) = raw {
                w = raw.min(w);
            }
        }
        (w, h, layout)
    }
}

/// `expandColumnWidths(widths, targetContentWidth)`.
pub fn expand_column_widths(widths: &[f64], target: f64) -> Vec<f64> {
    let base: Vec<f64> = widths.iter().map(|w| w.floor().max(1.0)).collect();
    let total: f64 = base.iter().sum();
    if total >= target {
        return base;
    }
    let columns = base.len() as f64;
    let extra = target - total;
    let shared = (extra / columns).floor();
    let remainder = extra % columns;
    base.iter().enumerate().map(|(i, w)| w + shared + if (i as f64) < remainder { 1.0 } else { 0.0 }).collect()
}

/// `allocateShrinkByWeight(shrinkable, targetShrink, mode)` (`sqrt` when `sqrt`, else linear).
pub fn allocate_shrink_by_weight(shrinkable: &[f64], target: f64, sqrt: bool) -> Vec<f64> {
    let mut shrink = vec![0f64; shrinkable.len()];
    if target <= 0.0 {
        return shrink;
    }
    let weights: Vec<f64> = shrinkable.iter().map(|&v| if v <= 0.0 { 0.0 } else if sqrt { v.sqrt() } else { v }).collect();
    let total: f64 = weights.iter().fold(0.0, |s, w| s + w);
    if total <= 0.0 {
        return shrink;
    }
    let mut fractions = vec![0f64; shrinkable.len()];
    let mut used = 0f64;
    for i in 0..shrinkable.len() {
        if shrinkable[i] <= 0.0 || weights[i] <= 0.0 {
            continue;
        }
        let exact = (weights[i] / total) * target;
        let whole = shrinkable[i].min(exact.floor());
        shrink[i] = whole;
        fractions[i] = exact - whole;
        used += whole;
    }
    let mut remaining = target - used;
    while remaining > 0.0 {
        let mut best: Option<usize> = None;
        let mut best_fraction = -1.0;
        for i in 0..shrinkable.len() {
            if shrinkable[i] - shrink[i] <= 0.0 {
                continue;
            }
            if best.is_none()
                || fractions[i] > best_fraction
                || (fractions[i] == best_fraction && best.is_some_and(|b| shrinkable[i] > shrinkable[b]))
            {
                best = Some(i);
                best_fraction = fractions[i];
            }
        }
        let Some(b) = best else { break };
        shrink[b] += 1.0;
        fractions[b] = 0.0;
        remaining -= 1.0;
    }
    shrink
}

/// `computeOffsets(parts, startBoundary, endBoundary, includeInnerBoundaries, innerGap)`.
pub fn compute_offsets(parts: &[f64], start: bool, end: bool, inner: bool, gap: f64) -> Vec<f64> {
    let mut offsets = vec![if start { 0.0 } else { -1.0 }];
    let mut cursor = offsets[0];
    for i in 0..parts.len() {
        let separator = if i < parts.len() - 1 {
            if inner { 1.0 } else { gap }
        } else if end {
            1.0
        } else {
            0.0
        };
        cursor += parts[i] + separator;
        offsets.push(cursor);
    }
    offsets
}
