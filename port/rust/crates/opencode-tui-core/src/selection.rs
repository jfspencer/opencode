//! The renderer's selection: `lib/selection.ts` (`Selection`, `convertGlobalToLocalSelection`) and the renderer's
//! `startSelection`, `updateSelection`, `clearSelection`, `finishSelection`, `requestSelectionUpdate` and
//! `getSelection`, which walk the selectable renderables under the selection's bounds
//! (`walkSelectableRenderables` over `getObjectsInViewport`) and tell each one through its
//! `onSelectionChanged`. Mouse selection (press, drag, release, ctrl+click extending, double and triple clicks)
//! is in the renderer's mouse dispatch.
//!
//! Renderables take part through [`Element::should_start_selection`], [`Element::on_selection_changed`],
//! [`Element::selected_text`] and [`Element::has_selection`]: a `<text>` selects in its scene text, a
//! `<textarea>` in its editor view, any other kind through its `Kind` methods, whose defaults are its superclass
//! part's (`Kind::base_kind`), so a TextRenderable subclass selects as a `<text>`.
use std::cell::RefCell;
use std::time::Instant;

pub use opencode_tui_text::SelectionBehavior;

use crate::layout;
use crate::renderables::text::TextKind;
use crate::renderables::textarea;
use crate::tree::{Element, FlexDirection, Kind};

/// `LocalSelectionBounds`: a selection in a renderable's cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalSelection {
    pub anchor_x: i32,
    pub anchor_y: i32,
    pub focus_x: i32,
    pub focus_y: i32,
    pub is_active: bool,
    pub behavior: SelectionBehavior,
}

/// The selection as `onSelectionChanged(selection)` sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionEvent {
    pub anchor: (i32, i32),
    pub focus: (i32, i32),
    pub is_active: bool,
    pub is_start: bool,
    pub is_dragging: bool,
    pub behavior: SelectionBehavior,
}

impl SelectionEvent {
    /// `convertGlobalToLocalSelection(selection, x, y)`.
    pub fn local(&self, x: i32, y: i32) -> Option<LocalSelection> {
        if !self.is_active {
            return None;
        }
        Some(LocalSelection {
            anchor_x: self.anchor.0 - x,
            anchor_y: self.anchor.1 - y,
            focus_x: self.focus.0 - x,
            focus_y: self.focus.1 - y,
            is_active: true,
            behavior: self.behavior,
        })
    }
}

/// `convertGlobalToLocalSelection` of an optional selection.
pub fn local_selection(selection: Option<&SelectionEvent>, x: i32, y: i32) -> Option<LocalSelection> {
    selection.and_then(|s| s.local(x, y))
}

/// `Selection`.
struct Selection {
    /// `SelectionAnchor`: the anchor relative to its renderable, which may move.
    anchor_el: Element,
    anchor_rel: (i32, i32),
    focus: (i32, i32),
    selected: Vec<Element>,
    touched: Vec<Element>,
    is_active: bool,
    is_dragging: bool,
    is_start: bool,
    behavior: SelectionBehavior,
}

impl Selection {
    fn anchor(&self) -> (i32, i32) {
        (self.anchor_el.x() + self.anchor_rel.0, self.anchor_el.y() + self.anchor_rel.1)
    }

    /// `bounds`: inclusive of anchor and focus.
    fn bounds(&self) -> (i32, i32, i32, i32) {
        let (ax, ay) = self.anchor();
        let (fx, fy) = self.focus;
        let (min_x, max_x, min_y, max_y) = (ax.min(fx), ax.max(fx), ay.min(fy), ay.max(fy));
        (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)
    }

    fn event(&self) -> SelectionEvent {
        SelectionEvent {
            anchor: self.anchor(),
            focus: self.focus,
            is_active: self.is_active,
            is_start: self.is_start,
            is_dragging: self.is_dragging,
            behavior: self.behavior,
        }
    }
}

struct LastClick {
    count: u32,
    time: Instant,
    x: i32,
    y: i32,
    num: u32,
}

#[derive(Default)]
struct State {
    current: Option<Selection>,
    containers: Vec<Element>,
    last_click: Option<LastClick>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

const CLICK_REPEAT_INTERVAL_MS: u128 = 500;

/// A snapshot of the current selection (`getSelection()`).
#[derive(Clone, Debug)]
pub struct SelectionInfo {
    pub event: SelectionEvent,
    /// `selectedRenderables`.
    pub selected: Vec<Element>,
}

impl SelectionInfo {
    pub fn is_start(&self) -> bool {
        self.event.is_start
    }
    pub fn is_dragging(&self) -> bool {
        self.event.is_dragging
    }
    pub fn behavior(&self) -> SelectionBehavior {
        self.event.behavior
    }

    /// `getSelectedText()`: the selected renderables' text in reading order, segments on one row joined.
    pub fn selected_text(&self) -> String {
        let mut renderables: Vec<(i32, i32, Element)> =
            self.selected.iter().filter(|el| !el.is_destroyed()).map(|el| (el.y(), el.x(), *el)).collect();
        renderables.sort_by_key(|(y, x, _)| (*y, *x));
        let mut lines: Vec<(i32, Vec<(i32, String)>)> = Vec::new();
        for (y, x, el) in renderables {
            let text = el.selected_text();
            if text.is_empty() {
                continue;
            }
            for (index, line) in text.split('\n').enumerate() {
                let row = y + index as i32;
                let segment = (x, line.to_string());
                match lines.iter_mut().find(|(r, _)| *r == row) {
                    Some((_, segments)) => segments.push(segment),
                    None => lines.push((row, vec![segment])),
                }
            }
        }
        lines.sort_by_key(|(row, _)| *row);
        lines
            .into_iter()
            .map(|(_, mut segments)| {
                segments.sort_by_key(|(x, _)| *x);
                segments.into_iter().map(|(_, text)| text).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `getSelection()`.
pub fn get_selection() -> Option<SelectionInfo> {
    with(|s| s.current.as_ref().map(|sel| SelectionInfo { event: sel.event(), selected: sel.selected.clone() }))
}

/// `hasSelection`.
pub fn has_selection() -> bool {
    with(|s| s.current.is_some())
}

/// `clearSelection()` (also forgets the click history).
pub fn clear_selection() {
    clear_selection_state();
    with(|s| s.last_click = None);
}

fn clear_selection_state() {
    let touched = with(|s| {
        s.containers.clear();
        s.current.take().map(|sel| sel.touched).unwrap_or_default()
    });
    for el in touched {
        if el.selectable() && !el.is_destroyed() {
            el.on_selection_changed(None);
        }
    }
}

/// `startSelection(renderable, x, y, behavior)`: a new selection anchored at a screen cell of `el`.
pub fn start_selection(el: Element, x: i32, y: i32, behavior: SelectionBehavior) {
    if !el.selectable() {
        return;
    }
    clear_selection_state();
    let container = el.parent().or_else(crate::tree::root);
    let anchor_rel = (x - el.x(), y - el.y());
    with(|s| {
        if let Some(container) = container {
            s.containers.push(container);
        }
        s.current = Some(Selection {
            anchor_el: el,
            anchor_rel,
            focus: (x, y),
            selected: Vec::new(),
            touched: Vec::new(),
            is_active: true,
            is_dragging: true,
            is_start: true,
            behavior,
        });
    });
    notify_selectables();
}

/// `nextClickBehavior`: a press near the last one on the same renderable within the repeat interval selects
/// words, then lines.
pub(crate) fn next_click_behavior(el: Element, x: i32, y: i32) -> SelectionBehavior {
    let num = el.num();
    let now = Instant::now();
    let count = with(|s| {
        let continued = s.last_click.as_ref().is_some_and(|last| {
            last.num == num
                && now.duration_since(last.time).as_millis() <= CLICK_REPEAT_INTERVAL_MS
                && (x - last.x).abs().max((y - last.y).abs()) <= 1
        });
        let count = if continued { (s.last_click.as_ref().map_or(0, |l| l.count) + 1).min(3) } else { 1 };
        s.last_click = Some(LastClick { count, time: now, x, y, num });
        count
    });
    match count {
        1 => SelectionBehavior::Cell,
        2 => SelectionBehavior::Word,
        _ => SelectionBehavior::Line,
    }
}

/// `updateSelection(currentRenderable, x, y, { finishDragging })`: moves the focus and re-walks.
pub fn update_selection(current: Option<Element>, x: i32, y: i32, finish_dragging: bool) {
    let containers = with(|s| {
        let sel = s.current.as_mut()?;
        sel.is_start = false;
        sel.focus = (x, y);
        if finish_dragging {
            sel.is_dragging = false;
        }
        Some(s.containers.clone())
    });
    let Some(mut containers) = containers else { return };
    if let Some(&top) = containers.last() {
        let within = current.is_some_and(|el| is_within(el, top));
        if !within {
            if let Some(parent) = top.parent().or_else(crate::tree::root) {
                containers.push(parent);
            }
        } else if let Some(el) = current.filter(|_| containers.len() > 1) {
            let index = containers
                .iter()
                .position(|c| *c == el)
                .or_else(|| el.parent().or_else(crate::tree::root).and_then(|p| containers.iter().position(|c| *c == p)));
            if let Some(index) = index.filter(|i| *i + 1 < containers.len()) {
                containers.truncate(index + 1);
            }
        }
    }
    with(|s| s.containers = containers);
    notify_selectables();
}

/// `finishSelection()`: the drag ends.
pub(crate) fn finish_selection() {
    let had = with(|s| match s.current.as_mut() {
        Some(sel) => {
            sel.is_dragging = false;
            true
        }
        None => false,
    });
    if had {
        notify_selectables();
    }
}

/// Sets the current selection's behavior (a keyboard extension of a word or line selection works in cells).
pub fn set_behavior(behavior: SelectionBehavior) {
    with(|s| {
        if let Some(sel) = s.current.as_mut() {
            sel.behavior = behavior;
        }
    });
}

/// `requestSelectionUpdate()`: while dragging, the selection follows the pointer again (a textarea scrolled
/// under it).
pub fn request_selection_update() {
    let dragging = with(|s| s.current.as_ref().is_some_and(|sel| sel.is_dragging));
    if !dragging {
        return;
    }
    if let Some((x, y)) = crate::renderer::pointer() {
        update_selection(crate::renderer::hit_test_at(x, y), x, y, false);
    }
}

/// `isDragging` of the current selection.
pub(crate) fn is_dragging() -> bool {
    with(|s| s.current.as_ref().is_some_and(|sel| sel.is_dragging))
}

/// Marks the current selection as dragging (ctrl+click extends it).
pub(crate) fn set_dragging() {
    with(|s| {
        if let Some(sel) = s.current.as_mut() {
            sel.is_dragging = true;
        }
    });
}

fn is_within(el: Element, container: Element) -> bool {
    let mut current = Some(el);
    while let Some(c) = current {
        if c == container {
            return true;
        }
        current = c.parent();
    }
    false
}

/// `notifySelectablesOfSelectionChange`.
fn notify_selectables() {
    let Some((event, bounds, container, previous)) = with(|s| {
        let sel = s.current.as_ref()?;
        let container = s.containers.last().copied().or_else(crate::tree::root)?;
        Some((sel.event(), sel.bounds(), container, sel.touched.clone()))
    }) else {
        return;
    };
    let mut selected = Vec::new();
    let mut touched = Vec::new();
    walk(container, bounds, &event, &mut selected, &mut touched);
    for el in previous {
        if !touched.contains(&el) && !el.is_destroyed() {
            el.on_selection_changed(None);
        }
    }
    with(|s| {
        if let Some(sel) = s.current.as_mut() {
            sel.selected = selected;
            sel.touched = touched;
        }
    });
}

/// `walkSelectableRenderables`.
fn walk(
    container: Element,
    bounds: (i32, i32, i32, i32),
    event: &SelectionEvent,
    selected: &mut Vec<Element>,
    touched: &mut Vec<Element>,
) {
    for child in objects_in_viewport(bounds, container) {
        if child.selectable() {
            if child.on_selection_changed(Some(event)) {
                selected.push(child);
            }
            touched.push(child);
        }
        if !child.children().is_empty() {
            walk(child, bounds, event, selected, touched);
        }
    }
}

/// `getObjectsInViewport(bounds, container.getChildrenSortedByPrimaryAxis(), container.primaryAxis, 0, 0)`.
fn objects_in_viewport(bounds: (i32, i32, i32, i32), container: Element) -> Vec<Element> {
    let (vx, vy, vw, vh) = bounds;
    if vw <= 0 || vh <= 0 {
        return Vec::new();
    }
    let row = matches!(primary_axis(container), FlexDirection::Row);
    struct Item {
        el: Element,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        z: i32,
    }
    let mut items: Vec<Item> = container
        .children()
        .into_iter()
        .filter(|c| !c.is_text_node())
        .map(|el| {
            let (x, y) = el.screen_position();
            let g = el.geometry();
            Item { el, x, y, w: g.width as f64, h: g.height as f64, z: el.z_index() }
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    items.sort_by(|a, b| {
        let (ka, kb) = if row { (a.x, b.x) } else { (a.y, b.y) };
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let (top, bottom, left, right) = (vy as f64, (vy + vh) as f64, vx as f64, (vx + vw) as f64);
    let (vp_start, vp_end) = if row { (left, right) } else { (top, bottom) };
    let start = |i: &Item| if row { i.x } else { i.y };
    let end = |i: &Item| if row { i.x + i.w } else { i.y + i.h };
    let (mut lo, mut hi, mut candidate) = (0i64, items.len() as i64 - 1, -1i64);
    while lo <= hi {
        let mid = (lo + hi) >> 1;
        let item = &items[mid as usize];
        if end(item) < vp_start {
            lo = mid + 1;
        } else if start(item) > vp_end {
            hi = mid - 1;
        } else {
            candidate = mid;
            break;
        }
    }
    if candidate == -1 {
        candidate = if lo > 0 { lo - 1 } else { 0 };
    }
    let mut first = candidate as usize;
    let mut gaps = 0;
    while first > 0 {
        if end(&items[first - 1]) <= vp_start {
            gaps += 1;
            if gaps >= 50 {
                break;
            }
        } else {
            gaps = 0;
        }
        first -= 1;
    }
    let mut last = candidate as usize + 1;
    while last < items.len() && start(&items[last]) < vp_end {
        last += 1;
    }
    let mut visible: Vec<&Item> = Vec::new();
    for item in &items[first..last] {
        if end(item) <= vp_start {
            continue;
        }
        if start(item) >= vp_end {
            break;
        }
        let (cross_start, cross_end, lo, hi) =
            if row { (item.y, item.y + item.h, top, bottom) } else { (item.x, item.x + item.w, left, right) };
        if cross_end < lo || cross_start > hi {
            continue;
        }
        visible.push(item);
    }
    visible.sort_by_key(|item| item.z);
    visible.into_iter().map(|item| item.el).collect()
}

/// `primaryAxis`.
fn primary_axis(el: Element) -> FlexDirection {
    let mut direction = FlexDirection::Column;
    el.with_yoga(|y| {
        direction = match y.get_enum(layout::ENUM_FLEX_DIRECTION) {
            2 | 3 => FlexDirection::Row,
            _ => FlexDirection::Column,
        }
    });
    direction
}

impl Element {
    // A kind that reads as a `TextKind` (`create_text_with` subclasses) selects as a `<text>` does.
    fn with_selection_kind<R>(self, text: impl FnOnce(&mut TextKind) -> R, other: impl FnOnce(&mut dyn Kind) -> R) -> Option<R> {
        self.with_dyn_kind(|k| {
            if k.as_any().is::<TextKind>() {
                return text(k.as_any_mut().downcast_mut::<TextKind>().expect("a text kind"));
            }
            other(k)
        })
    }

    /// `shouldStartSelection(x, y)`.
    pub fn should_start_selection(self, x: i32, y: i32) -> bool {
        if let Some(start) = textarea::selection_should_start(self, x, y) {
            return start;
        }
        self.with_selection_kind(|t| t.selection_should_start(self, x, y), |k| k.should_start_selection(self, x, y))
            .unwrap_or(false)
    }

    /// `onSelectionChanged(selection)`: whether the node now holds a selection.
    pub fn on_selection_changed(self, selection: Option<&SelectionEvent>) -> bool {
        if let Some(has) = textarea::selection_changed(self, selection) {
            return has;
        }
        self.with_selection_kind(|t| t.selection_changed(self, selection), |k| k.on_selection_changed(self, selection))
            .unwrap_or(false)
    }

    /// `getSelectedText()`.
    pub fn selected_text(self) -> String {
        if let Some(text) = textarea::selection_text(self) {
            return text;
        }
        self.with_selection_kind(|t| t.selection_text(), |k| k.selected_text(self)).unwrap_or_default()
    }

    /// `hasSelection()`.
    pub fn has_selection(self) -> bool {
        if let Some(has) = textarea::selection_has(self) {
            return has;
        }
        self.with_selection_kind(|t| t.selection_has(), |k| k.has_selection(self)).unwrap_or(false)
    }
}
