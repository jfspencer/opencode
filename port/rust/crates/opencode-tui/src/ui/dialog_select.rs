//! `ui/dialog-select.tsx` with `ui/select-controller.ts`: a filterable list dialog (fuzzy search over title,
//! category and search text; groups by category or a flat list; the selection that follows the current value,
//! survives option changes and wraps; scrolling that keeps it in view; per-option details, footers, gutters and
//! margins; actions bound to keys and listed in the footer; the "…" overflow row and the detail line of the
//! "new-designs" experiment), and `DialogSelectChoice`.
//!
//! Divergence: rows are reconciled by position (`<Index>`); TS recreates them whenever the groups recompute (its
//! `<For>` keys the new group tuples). The rows show the same thing either way.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, input, scrollbox, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::scrollbox::{ScrollBarOptions, ScrollUnit};
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, MouseEventType, Overflow};

use crate::ctx::tui;
use crate::fuzzysort;
use crate::keymap::{Bind, Command, Layer, create_layer};
use crate::theme::{ResolvedTheme, SurfaceName};
use crate::ui::dialog::{
    DIALOG_INSET, DIALOG_LIST_INSET, DialogContext, FooterAction, FooterActions, FooterProps, HeaderProps, LayoutProps,
    dialog_bleed, dialog_footer, dialog_header, dialog_layout, dialog_width, use_dialog, use_new_designs,
};
use crate::ui::locale;
use crate::ui::theme::use_theme;
use crate::util::rgba;

fn point(v: f32) -> Option<Value> {
    Some(Value::Point(v))
}

/// `reconcileSelection(selected, count)`.
pub fn reconcile_selection(selected: usize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    selected.min(count - 1)
}

/// `moveSelection(selected, { count, delta, policy })`.
pub fn move_selection(selected: usize, count: usize, delta: i64, wrap: bool) -> usize {
    if count == 0 {
        return 0;
    }
    let next = selected as i64 + delta;
    if !wrap {
        return next.clamp(0, count as i64 - 1) as usize;
    }
    if next < 0 {
        return count - 1;
    }
    if next >= count as i64 {
        return 0;
    }
    next as usize
}

/// A footer or search footer: text, or an element inside the footer's `<text>` (a span).
#[derive(Clone)]
pub enum Footer {
    Text(String),
    View(Rc<dyn Fn() -> View>),
}

impl PartialEq for Footer {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Footer::Text(a), Footer::Text(b)) => a == b,
            (Footer::View(a), Footer::View(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// `truncateTitle`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TruncateTitle {
    #[default]
    Right,
    Left,
    None,
}

/// `detail`: muted text, or an element made each time the option becomes selected.
#[derive(Clone)]
pub enum Detail {
    Text(String),
    View(Rc<dyn Fn() -> View>),
}

/// `DialogSelectOption<T>`.
#[derive(Clone)]
pub struct SelectOption<T> {
    pub title: String,
    pub title_view: Option<Rc<dyn Fn() -> View>>,
    pub value: T,
    pub description: Option<String>,
    pub search_text: Option<String>,
    pub search_footer: Option<Footer>,
    pub details: Vec<String>,
    pub footer: Option<Footer>,
    pub footer_color: Option<RGBA>,
    /// Cells the footer extends into the row's right padding.
    pub footer_overhang: Option<f32>,
    pub title_width: Option<usize>,
    pub truncate_title: TruncateTitle,
    pub category: Option<String>,
    pub disabled: bool,
    pub bg: Option<RGBA>,
    pub fg: Option<RGBA>,
    pub gutter: Option<Rc<dyn Fn(RGBA) -> View>>,
    pub margin: Option<Rc<dyn Fn() -> View>>,
    pub detail: Option<Detail>,
    pub on_select: Option<Rc<dyn Fn(&DialogContext)>>,
}

impl<T> SelectOption<T> {
    pub fn new(title: impl Into<String>, value: T) -> SelectOption<T> {
        SelectOption {
            title: title.into(),
            title_view: None,
            value,
            description: None,
            search_text: None,
            search_footer: None,
            details: Vec::new(),
            footer: None,
            footer_color: None,
            footer_overhang: None,
            title_width: None,
            truncate_title: TruncateTitle::Right,
            category: None,
            disabled: false,
            bg: None,
            fg: None,
            gutter: None,
            margin: None,
            detail: None,
            on_select: None,
        }
    }
}

fn same_rc<A: ?Sized>(a: &Option<Rc<A>>, b: &Option<Rc<A>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}

impl<T: PartialEq> PartialEq for SelectOption<T> {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title
            && self.value == other.value
            && self.description == other.description
            && self.search_text == other.search_text
            && self.search_footer == other.search_footer
            && self.details == other.details
            && self.footer == other.footer
            && self.footer_color == other.footer_color
            && self.footer_overhang == other.footer_overhang
            && self.title_width == other.title_width
            && self.truncate_title == other.truncate_title
            && self.category == other.category
            && self.disabled == other.disabled
            && self.bg == other.bg
            && self.fg == other.fg
            && same_rc(&self.title_view, &other.title_view)
            && same_rc(&self.gutter, &other.gutter)
            && same_rc(&self.margin, &other.margin)
            && same_rc(&self.on_select, &other.on_select)
            && match (&self.detail, &other.detail) {
                (None, None) => true,
                (Some(Detail::Text(a)), Some(Detail::Text(b))) => a == b,
                (Some(Detail::View(a)), Some(Detail::View(b))) => Rc::ptr_eq(a, b),
                _ => false,
            }
    }
}

/// Footer placement while the "new-designs" experiment is off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Footer group under the "new-designs" experiment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Primary,
    Secondary,
}

/// `DialogSelectAction<T>`.
#[derive(Clone)]
pub struct SelectAction<T> {
    pub command: String,
    /// A title that follows the selected option, e.g. "favorite" / "unfavorite".
    pub title: Rc<dyn Fn(Option<&SelectOption<T>>) -> String>,
    pub bind: Option<String>,
    pub side: Option<Side>,
    pub group: Option<Group>,
    pub hidden: bool,
    pub disabled: Option<Rc<dyn Fn(Option<&SelectOption<T>>) -> bool>>,
    /// `selection: "none"`: a dialog-level action that runs without a selected option.
    pub selection_none: bool,
    pub on_trigger: Rc<dyn Fn(Option<&SelectOption<T>>)>,
}

impl<T> SelectAction<T> {
    /// An action on the selected option.
    pub fn new(command: &str, title: &str, on_trigger: impl Fn(&SelectOption<T>) + 'static) -> SelectAction<T> {
        let title = title.to_string();
        SelectAction {
            command: command.to_string(),
            title: Rc::new(move |_| title.clone()),
            bind: None,
            side: None,
            group: None,
            hidden: false,
            disabled: None,
            selection_none: false,
            on_trigger: Rc::new(move |option| {
                if let Some(option) = option {
                    on_trigger(option)
                }
            }),
        }
    }

    /// A dialog-level action (`selection: "none"`).
    pub fn dialog(command: &str, title: &str, on_trigger: impl Fn() + 'static) -> SelectAction<T> {
        let title = title.to_string();
        SelectAction {
            command: command.to_string(),
            title: Rc::new(move |_| title.clone()),
            bind: None,
            side: None,
            group: None,
            hidden: false,
            disabled: None,
            selection_none: true,
            on_trigger: Rc::new(move |_| on_trigger()),
        }
    }
}

/// `footerHints[number]`.
#[derive(Clone, Debug, PartialEq)]
pub struct FooterHint {
    pub title: String,
    pub label: String,
    pub side: Option<Side>,
    pub group: Option<Group>,
}

/// `DialogSelectRef<T>`.
#[derive(Clone)]
pub struct SelectRef<T: 'static> {
    filter: Signal<String>,
    filtered: Memo<Vec<SelectOption<T>>>,
    selected: Memo<Option<SelectOption<T>>>,
    set_filter: Rc<dyn Fn(String)>,
    move_to: Rc<dyn Fn(&T)>,
}

impl<T: Clone + PartialEq + 'static> SelectRef<T> {
    pub fn filter(&self) -> String {
        self.filter.get()
    }
    pub fn filtered(&self) -> Vec<SelectOption<T>> {
        self.filtered.get()
    }
    pub fn selected(&self) -> Option<SelectOption<T>> {
        self.selected.get()
    }
    pub fn set_filter(&self, value: &str) {
        (self.set_filter)(value.to_string())
    }
    pub fn move_to(&self, value: &T) {
        (self.move_to)(value)
    }
}

type OptionsFn<T> = Rc<dyn Fn() -> Vec<SelectOption<T>>>;

/// `DialogSelectProps<T>`.
pub struct SelectProps<T: 'static> {
    pub title: Rc<dyn Fn() -> String>,
    pub title_view: Option<Rc<dyn Fn() -> View>>,
    pub description: Option<Rc<dyn Fn() -> Option<String>>>,
    pub footer_gap: Option<f32>,
    pub placeholder: Option<String>,
    pub footer: Option<Rc<dyn Fn() -> View>>,
    pub empty_view: Option<Rc<dyn Fn() -> View>>,
    pub no_match_view: Option<Rc<dyn Fn() -> View>>,
    pub options: OptionsFn<T>,
    pub flat: bool,
    pub filter_threshold: Option<f64>,
    pub on_ref: Option<Rc<dyn Fn(SelectRef<T>)>>,
    pub on_move: Option<Rc<dyn Fn(&SelectOption<T>)>>,
    pub on_filter: Option<Rc<dyn Fn(&str)>>,
    pub on_select: Option<Rc<dyn Fn(&SelectOption<T>)>>,
    pub on_cancel: Option<Rc<dyn Fn()>>,
    pub skip_filter: bool,
    pub search: Option<Rc<dyn Fn(&str) -> Vec<SelectOption<T>>>>,
    /// `renderFilter`: false hides the search input.
    pub render_filter: bool,
    pub locked: Option<Rc<dyn Fn() -> bool>>,
    pub preserve_selection: bool,
    pub actions: Option<Rc<dyn Fn() -> Vec<SelectAction<T>>>>,
    pub footer_hints: Vec<FooterHint>,
    pub bindings: Vec<Command>,
    pub current: Option<Rc<dyn Fn() -> Option<T>>>,
    pub focus_target: Option<Rc<dyn Fn() -> Option<T>>>,
    /// `focusCurrent`: `Some(false)` keeps the selection off the current value.
    pub focus_current: Option<bool>,
    pub section_navigation: bool,
    pub search_in_title: bool,
    pub title_placeholder: Option<String>,
}

impl<T: 'static> SelectProps<T> {
    pub fn new(title: impl Into<String>, options: impl Fn() -> Vec<SelectOption<T>> + 'static) -> SelectProps<T> {
        let title = title.into();
        SelectProps {
            title: Rc::new(move || title.clone()),
            title_view: None,
            description: None,
            footer_gap: None,
            placeholder: None,
            footer: None,
            empty_view: None,
            no_match_view: None,
            options: Rc::new(options),
            flat: false,
            filter_threshold: None,
            on_ref: None,
            on_move: None,
            on_filter: None,
            on_select: None,
            on_cancel: None,
            skip_filter: false,
            search: None,
            render_filter: true,
            locked: None,
            preserve_selection: false,
            actions: None,
            footer_hints: Vec::new(),
            bindings: Vec::new(),
            current: None,
            focus_target: None,
            focus_current: None,
            section_navigation: false,
            search_in_title: false,
            title_placeholder: None,
        }
    }
}

/// `dialogSelectContentWidth(dialogWidth)`: scroll padding, row padding, the gutter, title padding and the gap.
pub fn dialog_select_content_width(width: u32) -> i64 {
    width as i64 - DIALOG_LIST_INSET as i64 * 2 - 4
}

/// `<DialogSelect>`.
pub fn dialog_select<T: Clone + PartialEq + 'static>(props: SelectProps<T>) -> View {
    let dialog = use_dialog();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let config = tui().config_context;
    let new_designs = use_new_designs();
    let scroll_acceleration = create_memo_equals(move || Accel(crate::ui::scroll::scroll_acceleration(&config.data())), None);
    let props = Rc::new(props);
    let locked = {
        let props = props.clone();
        Rc::new(move || props.locked.as_ref().is_some_and(|l| l()))
    };
    let current = {
        let props = props.clone();
        Rc::new(move || props.current.as_ref().and_then(|c| c()))
    };
    let focus_target = {
        let props = props.clone();
        Rc::new(move || props.focus_target.as_ref().and_then(|c| c()))
    };
    let has_search = props.search.is_some();
    let focus_current_off = props.focus_current == Some(false);

    let (selected_index, _) = create_signal(0usize);
    let (filter, _) = create_signal(String::new());
    let selection: Rc<RefCell<Option<(T, Option<String>)>>> = Rc::new(RefCell::new(None));
    let reset_selection = Rc::new(Cell::new(false));
    let pending_scroll: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
    let scroll: Rc<Cell<Option<Element>>> = Rc::new(Cell::new(None));

    // Read through on each use, as TS reads `props.options` (a getter): what it reads is tracked by its reader.
    let options: OptionsFn<T> = props.options.clone();

    let actions = {
        let props = props.clone();
        create_memo_equals(move || Rc::new(props.actions.as_ref().map(|a| a()).unwrap_or_default()), None)
    };
    let shown_actions = create_memo_equals(
        move || Rc::new(actions.get().iter().filter(|a| !a.hidden).cloned().collect::<Vec<_>>()),
        None,
    );
    let shortcuts = crate::keymap::use_shortcuts();
    let action_labels = create_memo(move || {
        shown_actions
            .get()
            .iter()
            .filter_map(|a| shortcuts.all(&a.command).map(|label| (a.command.clone(), label)))
            .collect::<Vec<_>>()
    });

    let filtered = {
        let props = props.clone();
        let options = options.clone();
        create_memo(move || {
            let query = filter.get();
            if let Some(search) = &props.search {
                return search(&query).into_iter().filter(|x| !x.disabled).collect::<Vec<_>>();
            }
            let all: Vec<SelectOption<T>> = options().into_iter().filter(|x| !x.disabled).collect();
            if props.skip_filter || !props.render_filter {
                return all;
            }
            let needle = query.to_lowercase();
            if needle.is_empty() {
                return all;
            }
            // Title matches weigh twice the category and search text: users search by the item's name.
            let title: &dyn Fn(&SelectOption<T>) -> Option<String> = &|o| Some(o.title.clone());
            let category: &dyn Fn(&SelectOption<T>) -> Option<String> = &|o| o.category.clone();
            let search_text: &dyn Fn(&SelectOption<T>) -> Option<String> = &|o| o.search_text.clone();
            let score = |r: &fuzzysort::KeysResult| r.keys[0].score() * 2.0 + r.keys[1].score() + r.keys[2].score();
            let results = fuzzysort::go_keys(
                &needle,
                &all,
                &[title, category, search_text],
                &fuzzysort::Options { threshold: props.filter_threshold.unwrap_or(0.0), limit: None, score_fn: Some(&score) },
            );
            results.into_iter().map(|r| all[r.index].clone()).collect()
        })
    };

    let flatten = {
        let flat = props.flat;
        create_memo(move || has_search || (flat && filter.with(|f| !f.is_empty())))
    };

    let grouped = create_memo(move || {
        let list = filtered.get();
        if flatten.get() {
            return if list.is_empty() { Vec::new() } else { vec![(String::new(), list)] };
        }
        let mut groups: Vec<(String, Vec<SelectOption<T>>)> = Vec::new();
        for option in list {
            let category = option.category.clone().unwrap_or_default();
            match groups.iter_mut().find(|(c, _)| *c == category) {
                Some((_, items)) => items.push(option),
                None => groups.push((category, vec![option])),
            }
        }
        groups
    });

    let flat = create_memo(move || grouped.get().into_iter().flat_map(|(_, options)| options).collect::<Vec<_>>());

    let rows = create_memo(move || {
        let groups = grouped.get();
        let headers: usize =
            groups.iter().enumerate().map(|(i, (category, _))| if category.is_empty() { 0 } else if i > 0 { 2 } else { 1 }).sum();
        flat.get().iter().fold(headers, |acc, option| acc + 1 + option.details.len())
    });

    let dimensions = tui().dimensions;
    let limit = create_memo(move || (dimensions.get().1 as i64 / 2) - 6);
    // Under new designs an overflowing list gives its last row to a "…" indicator.
    let overflows = {
        let new_designs = new_designs.clone();
        create_memo(move || new_designs() && rows.get() as i64 > limit.get())
    };
    let details_width = {
        let dialog = dialog.clone();
        create_memo(move || dialog_select_content_width(dialog_width(dialog.size()).min(dimensions.get().0)))
    };
    let (more_below, set_more_below) = create_signal(false);
    let ellipsis = move || overflows.get() && more_below.get();
    let height = create_memo(move || (rows.get() as i64).min(limit.get()) - if ellipsis() { 1 } else { 0 });
    let update_more_below: Rc<dyn Fn()> = {
        let scroll = scroll.clone();
        Rc::new(move || {
            let Some(s) = scroll.get().filter(|s| s.is_alive() && !s.is_destroyed()) else { return };
            set_more_below.set(s.scroll_top() + s.scroll_viewport().height < s.scroll_height());
        })
    };

    let selected = create_memo(move || {
        let index = selected_index.get();
        flat.with(|f| f.get(index).cloned())
    });

    let scroll_to_selection: Rc<dyn Fn(bool)> = {
        let scroll = scroll.clone();
        Rc::new(move |center: bool| {
            let Some(s) = scroll.get().filter(|s| s.is_alive()) else { return };
            let mut remaining = selected_index.get_untracked();
            let mut index = 0usize;
            // Locate the row by position because a unique renderable ID cannot currently be ensured.
            for (category, options) in grouped.get_untracked() {
                if !category.is_empty() {
                    index += 1;
                }
                if remaining < options.len() {
                    index += remaining;
                    break;
                }
                index += options.len();
                remaining -= options.len();
            }
            let Some(target) = s.children().get(index).copied() else { return };
            let y = (target.y() - s.y()) as f64;
            let height = s.height() as f64;
            if center {
                let offset = (height / 2.0).floor();
                s.scroll_by(y - offset, ScrollUnit::Absolute);
                return;
            }
            if y >= height {
                s.scroll_by(y - height + 1.0, ScrollUnit::Absolute);
            }
            if y < 0.0 {
                s.scroll_by(y, ScrollUnit::Absolute);
                let first = flat.with_untracked(|f| f.first().map(|o| o.value.clone()));
                let current = selected.get_untracked().map(|o| o.value);
                if first.is_some() && first == current {
                    s.scroll_to(0.0);
                }
            }
        })
    };

    let scroll_after_layout: Rc<dyn Fn(bool, T)> = {
        let pending_scroll = pending_scroll.clone();
        let scroll_to_selection = scroll_to_selection.clone();
        Rc::new(move |center: bool, value: T| {
            if let Some(token) = pending_scroll.take() {
                renderer::off_frame(token);
            }
            let pending = pending_scroll.clone();
            let scroll_to_selection = scroll_to_selection.clone();
            let token = renderer::on_frame(
                move || {
                    pending.set(None);
                    if selected.get_untracked().map(|o| o.value).as_ref() != Some(&value) {
                        return;
                    }
                    scroll_to_selection(center);
                },
                true,
            );
            pending_scroll.set(Some(token));
            renderer::request_render();
        })
    };

    let move_to: Rc<dyn Fn(usize, bool, bool)> = {
        let props = props.clone();
        let selection = selection.clone();
        let reset_selection = reset_selection.clone();
        let scroll_to_selection = scroll_to_selection.clone();
        let scroll_after_layout = scroll_after_layout.clone();
        Rc::new(move |next: usize, center: bool, preserve: bool| {
            selected_index.set(next);
            let option = selected.get_untracked();
            if let Some(option) = &option {
                *selection.borrow_mut() = Some((option.value.clone(), option.category.clone()));
                reset_selection.set(!preserve);
            }
            if let (Some(option), Some(on_move)) = (&option, &props.on_move) {
                on_move(option);
            }
            scroll_to_selection(center);
            // Rows below the viewport may not have been laid out since the list last changed: scroll again once
            // they are.
            if let Some(option) = option {
                scroll_after_layout(center, option.value);
            }
        })
    };

    // Follow the current value (or the focus target).
    {
        let props = props.clone();
        let selection = selection.clone();
        let scroll_after_layout = scroll_after_layout.clone();
        let focus_target = focus_target.clone();
        let current = current.clone();
        let target_defined = props.focus_target.is_some();
        let target = focus_target.clone();
        create_effect(on(
            move || {
                let value = target().or_else(|| current());
                let flat = if target().is_none() { None } else { Some(flat.get()) };
                (value, flat)
            },
            move |(value, _), _| {
                if has_search || focus_current_off {
                    return;
                }
                if target_defined
                    && focus_target().is_some()
                    && (props.preserve_selection || filter.with_untracked(|f| !f.is_empty()))
                {
                    return;
                }
                let Some(value) = value.clone() else { return };
                let index = flat.with_untracked(|f| f.iter().position(|o| o.value == value));
                if let Some(index) = index {
                    selected_index.set(index);
                    *selection.borrow_mut() =
                        flat.with_untracked(|f| f.get(index).map(|o| (o.value.clone(), o.category.clone())));
                    scroll_after_layout(true, value);
                }
            },
            false,
        ));
    }

    // Keep the selection on its option when the options change.
    {
        let props = props.clone();
        let selection = selection.clone();
        let reset_selection = reset_selection.clone();
        let scroll_after_layout = scroll_after_layout.clone();
        let focus_target = focus_target.clone();
        let current = current.clone();
        create_effect(on(
            move || options(),
            move |_, _| {
                if has_search {
                    return;
                }
                let anchor = focus_target().or_else(|| current());
                if !props.preserve_selection && (anchor.is_none() || focus_current_off) {
                    let count = flat.with_untracked(Vec::len);
                    if count == 0 {
                        return;
                    }
                    let next = reconcile_selection(selected_index.get_untracked(), count);
                    if next != selected_index.get_untracked() {
                        selected_index.set(next);
                    }
                    return;
                }
                if reset_selection.get() && filter.with_untracked(|f| !f.is_empty()) {
                    let Some(option) = flat.with_untracked(|f| f.first().cloned()) else { return };
                    selected_index.set(0);
                    *selection.borrow_mut() = Some((option.value, option.category));
                    return;
                }
                let previous = selection.borrow().clone();
                let Some((previous_value, previous_category)) = previous else {
                    if !focus_current_off {
                        if let Some(anchor) = &anchor {
                            let index = flat.with_untracked(|f| f.iter().position(|o| o.value == *anchor));
                            if let Some(index) = index {
                                selected_index.set(index);
                                *selection.borrow_mut() =
                                    flat.with_untracked(|f| f.get(index).map(|o| (o.value.clone(), o.category.clone())));
                                return;
                            }
                        }
                    }
                    let Some(option) = selected.get_untracked() else { return };
                    *selection.borrow_mut() = Some((option.value, option.category));
                    return;
                };
                let index = flat.with_untracked(|f| f.iter().position(|o| o.value == previous_value));
                if let Some(index) = index {
                    let option = flat.with_untracked(|f| f[index].clone());
                    let moved = index != selected_index.get_untracked() || option.category != previous_category;
                    selected_index.set(index);
                    *selection.borrow_mut() = Some((option.value.clone(), option.category.clone()));
                    if !moved {
                        return;
                    }
                    if !props.preserve_selection
                        && (anchor.is_none() || focus_current_off || filter.with_untracked(|f| !f.is_empty()))
                    {
                        return;
                    }
                    scroll_after_layout(false, option.value);
                    return;
                }
                let count = flat.with_untracked(Vec::len);
                let next = reconcile_selection(selected_index.get_untracked(), count);
                if count == 0 {
                    return;
                }
                selected_index.set(next);
                *selection.borrow_mut() = flat.with_untracked(|f| f.get(next).map(|o| (o.value.clone(), o.category.clone())));
            },
            false,
        ));
    }

    // With `search`: the selection follows the query's first result, the intended option, or its position.
    {
        let selection = selection.clone();
        let scroll_after_layout = scroll_after_layout.clone();
        let focus_target = focus_target.clone();
        let current = current.clone();
        create_effect(on(
            move || {
                let anchor = if focus_current_off { None } else { focus_target().or_else(|| current()) };
                (flat.get(), filter.get(), anchor)
            },
            move |(options, query, anchor), previous: Option<&(Vec<SelectOption<T>>, String, Option<T>)>| {
                if !has_search {
                    return;
                }
                let query_changed = previous.is_some_and(|p| p.1 != *query);
                let current_changed = anchor.is_some() && previous.is_none_or(|p| p.2 != *anchor);
                let intent: Option<T> = if query_changed && !query.is_empty() {
                    options.first().map(|o| o.value.clone())
                } else if (query_changed || current_changed) && anchor.is_some() {
                    anchor.clone()
                } else if query_changed {
                    options.get(reconcile_selection(selected_index.get_untracked(), options.len())).map(|o| o.value.clone())
                } else {
                    selection.borrow().as_ref().map(|(v, _)| v.clone())
                };
                let index = intent.as_ref().and_then(|v| options.iter().position(|o| o.value == *v));
                let next = index.unwrap_or_else(|| reconcile_selection(selected_index.get_untracked(), options.len()));
                let Some(option) = options.get(next).cloned() else { return };
                selected_index.set(next);
                *selection.borrow_mut() = Some((option.value.clone(), option.category.clone()));
                scroll_after_layout(query_changed || current_changed, option.value);
            },
            false,
        ));
    }
    {
        let pending_scroll = pending_scroll.clone();
        on_cleanup(move || {
            if let Some(token) = pending_scroll.take() {
                renderer::off_frame(token);
            }
        });
    }

    // A filter moves to the first match; an empty filter back to the current value.
    {
        let move_to = move_to.clone();
        let reset_selection = reset_selection.clone();
        let scroll_after_layout = scroll_after_layout.clone();
        let focus_target = focus_target.clone();
        let current = current.clone();
        create_effect(on(
            move || (filter.get(), focus_target().or_else(|| current())),
            move |(query, anchor), _| {
                if has_search {
                    return;
                }
                if !query.is_empty() {
                    reset_selection.set(true);
                    let Some(option) = flat.with_untracked(|f| f.first().cloned()) else { return };
                    move_to(0, true, false);
                    scroll_after_layout(true, option.value);
                    return;
                }
                let Some(anchor) = anchor.clone() else { return };
                if focus_current_off {
                    return;
                }
                let Some(index) = flat.with_untracked(|f| f.iter().position(|o| o.value == anchor)) else { return };
                move_to(index, true, true);
                scroll_after_layout(true, anchor);
            },
            false,
        ));
    }

    let movement: Rc<dyn Fn(i64)> = {
        let locked = locked.clone();
        let move_to = move_to.clone();
        Rc::new(move |direction: i64| {
            if locked() {
                return;
            }
            let count = flat.with_untracked(Vec::len);
            if count == 0 {
                return;
            }
            move_to(move_selection(selected_index.get_untracked(), count, direction, true), true, true);
        })
    };

    let move_section: Rc<dyn Fn(i64)> = {
        let locked = locked.clone();
        let move_to = move_to.clone();
        Rc::new(move |direction: i64| {
            if locked() {
                return;
            }
            let sections: Vec<(String, Vec<SelectOption<T>>)> =
                grouped.get_untracked().into_iter().filter(|(_, o)| !o.is_empty()).collect();
            if sections.is_empty() {
                return;
            }
            let category = selected.get_untracked().and_then(|o| o.category).unwrap_or_default();
            let current = sections.iter().position(|(c, _)| *c == category).map(|i| i as i64).unwrap_or(-1);
            let count = sections.len() as i64;
            let section = &sections[((current + direction + count) % count) as usize];
            let first = &section.1[0];
            let index = flat.with_untracked(|f| f.iter().position(|o| o == first)).unwrap_or(0);
            move_to(index, true, true);
        })
    };

    let submit: Rc<dyn Fn()> = {
        let locked = locked.clone();
        let props = props.clone();
        let dialog = dialog.clone();
        Rc::new(move || {
            if locked() {
                return;
            }
            let Some(option) = selected.get_untracked() else { return };
            if let Some(on_select) = &option.on_select {
                on_select(&dialog);
            }
            if let Some(on_select) = &props.on_select {
                on_select(&option);
            }
        })
    };

    let is_disabled = move |item: &SelectAction<T>| -> bool {
        let option = selected.get_untracked();
        if !item.selection_none && option.is_none() {
            return true;
        }
        item.disabled.as_ref().is_some_and(|d| d(option.as_ref()))
    };
    let trigger: Rc<dyn Fn(&SelectAction<T>)> = {
        let locked = locked.clone();
        Rc::new(move |item: &SelectAction<T>| {
            if locked() || is_disabled(item) {
                return;
            }
            if item.selection_none {
                (item.on_trigger)(None);
                return;
            }
            let Some(option) = selected.get_untracked() else { return };
            (item.on_trigger)(Some(&option));
        })
    };

    {
        let props = props.clone();
        let movement = movement.clone();
        let move_to = move_to.clone();
        let submit = submit.clone();
        let trigger = trigger.clone();
        let move_section = move_section.clone();
        let locked = locked.clone();
        create_layer(move || {
            let visible = shown_actions.get();
            let named = |id: &str, title: &str, run: Rc<dyn Fn()>| {
                Command::new(id, move || run()).title(title).group("Dialog")
            };
            let mut commands = vec![
                named("dialog.select.prev", "Previous item", { let m = movement.clone(); Rc::new(move || m(-1)) }),
                named("dialog.select.next", "Next item", { let m = movement.clone(); Rc::new(move || m(1)) }),
                named("dialog.select.page_up", "Page up", { let m = movement.clone(); Rc::new(move || m(-10)) }),
                named("dialog.select.page_down", "Page down", { let m = movement.clone(); Rc::new(move || m(10)) }),
                named("dialog.select.home", "First item", {
                    let move_to = move_to.clone();
                    let locked = locked.clone();
                    Rc::new(move || {
                        if !locked() {
                            move_to(0, false, true)
                        }
                    })
                }),
                named("dialog.select.end", "Last item", {
                    let move_to = move_to.clone();
                    let locked = locked.clone();
                    Rc::new(move || {
                        if !locked() {
                            move_to(flat.with_untracked(Vec::len).saturating_sub(1), false, true)
                        }
                    })
                }),
                named("dialog.select.submit", "Select item", submit.clone()),
            ];
            for item in visible.iter() {
                let trigger = trigger.clone();
                let action = item.clone();
                // Resolved without a selection so moving through the list does not rebuild the layer.
                let mut command = Command::new(&item.command, move || trigger(&action)).title((item.title)(None)).group("Dialog");
                if let Some(bind) = &item.bind {
                    command = command.bind(Bind::Key(bind.clone()));
                }
                commands.push(command);
            }
            commands.extend(props.bindings.iter().cloned());
            if let Some(on_cancel) = props.on_cancel.clone() {
                // TODO(port-tui): `renderer.getSelection()` clears a selection first.
                commands.push(
                    Command::inline("escape", move || {
                        on_cancel();
                        true
                    })
                    .title("Back")
                    .group("Dialog"),
                );
            }
            if props.section_navigation {
                let up = move_section.clone();
                let down = move_section.clone();
                commands.push(Command::inline("alt+up", move || { up(-1); true }).title("Previous section").group("Dialog"));
                commands.push(Command::inline("alt+down", move || { down(1); true }).title("Next section").group("Dialog"));
            }
            Layer { mode: Some("modal"), commands, ..Layer::default() }
        });
    }

    let input_el: Rc<Cell<Option<Element>>> = Rc::new(Cell::new(None));
    {
        let props = props.clone();
        let on_ref = props.on_ref.clone();
        let input_el = input_el.clone();
        let move_to = move_to.clone();
        let select_ref = SelectRef {
            filter,
            filtered,
            selected,
            set_filter: Rc::new(move |value: String| {
                if let Some(input) = input_el.get() {
                    input.set_input_value(&value);
                    return;
                }
                if filter.with_untracked(|f| *f == value) {
                    return;
                }
                batch(|| {
                    filter.set(value.clone());
                    if let Some(on_filter) = &props.on_filter {
                        on_filter(&value);
                    }
                });
            }),
            move_to: Rc::new(move |value: &T| {
                if let Some(index) = flat.with_untracked(|f| f.iter().position(|o| o.value == *value)) {
                    move_to(index, true, true);
                }
            }),
        };
        on_mount(move || {
            if let Some(on_ref) = on_ref {
                on_ref(select_ref);
            }
        });
    }

    let footer_actions = {
        let props = props.clone();
        let trigger = trigger.clone();
        move || -> Vec<(Option<Side>, Group, FooterAction)> {
            let labels = action_labels.get();
            let mut out: Vec<(Option<Side>, Group, FooterAction)> = shown_actions
                .get()
                .iter()
                .filter_map(|item| {
                    let label = labels.iter().find(|(c, _)| *c == item.command).map(|(_, l)| l.clone()).unwrap_or_default();
                    if label.is_empty() {
                        return None;
                    }
                    let option = selected.get();
                    let trigger = trigger.clone();
                    let action = item.clone();
                    Some((
                        item.side,
                        item.group.unwrap_or(if item.selection_none { Group::Secondary } else { Group::Primary }),
                        FooterAction {
                            title: (item.title)(option.as_ref()),
                            key: Some(label),
                            command: None,
                            on_trigger: Some(Rc::new(move || trigger(&action))),
                            disabled: {
                                let disabled = !item.selection_none && option.is_none();
                                disabled || item.disabled.as_ref().is_some_and(|d| d(option.as_ref()))
                            },
                            color: None,
                        },
                    ))
                })
                .collect();
            for hint in &props.footer_hints {
                out.push((
                    hint.side,
                    hint.group.unwrap_or(Group::Secondary),
                    FooterAction { title: hint.title.clone(), key: Some(hint.label.clone()), ..FooterAction::default() },
                ));
            }
            out
        }
    };
    let footer_actions = Rc::new(footer_actions);
    let visible_count = {
        let footer_actions = footer_actions.clone();
        move || footer_actions().len()
    };

    let search_in_title = {
        let props = props.clone();
        let new_designs = new_designs.clone();
        Rc::new(move || new_designs() && props.search_in_title && props.render_filter)
    };

    let search_input: Rc<dyn Fn() -> View> = {
        let props = props.clone();
        let input_el = input_el.clone();
        let locked = locked.clone();
        let new_designs = new_designs.clone();
        let search_in_title = search_in_title.clone();
        Rc::new(move || {
            let el = input();
            el.set_flex_grow(Some(1.0));
            {
                let props = props.clone();
                let locked = locked.clone();
                el.set_on_input(Some(Rc::new(move |value: String| {
                    if locked() {
                        return;
                    }
                    batch(|| {
                        filter.set(value.clone());
                        if let Some(on_filter) = &props.on_filter {
                            on_filter(&value);
                        }
                    });
                })));
            }
            bind_always(move || rgba(theme.get().background.formfield.focused), move |c| el.set_focused_background_color(c));
            {
                // Terminals paint the glyph under a block cursor in their own color: until the user types, match the
                // highlighted row so the placeholder's first letter shows through.
                let new_designs = new_designs.clone();
                bind_always(
                    move || {
                        let t = theme.get();
                        if !new_designs() {
                            return rgba(t.text.formfield.focused);
                        }
                        if filter.with(|f| !f.is_empty()) { rgba(t.text.base) } else { rgba(t.background.action.primary.focused) }
                    },
                    move |c| el.set_cursor_color_prop(c),
                );
            }
            {
                let new_designs = new_designs.clone();
                bind_always(
                    move || {
                        let t = theme.get();
                        if new_designs() { rgba(t.text.formfield.base) } else { rgba(t.text.formfield.focused) }
                    },
                    move |c| el.set_focused_text_color(c),
                );
            }
            input_el.set(Some(el));
            renderer::set_timeout(1, move || {
                if el.is_alive() && !el.is_destroyed() {
                    el.focus();
                }
            });
            {
                let props = props.clone();
                let search_in_title = search_in_title.clone();
                bind(
                    move || {
                        (if search_in_title() { props.title_placeholder.clone() } else { None })
                            .or_else(|| props.placeholder.clone())
                            .unwrap_or_else(|| "Search".to_string())
                    },
                    move |p| el.set_placeholder(&p),
                );
            }
            bind_always(move || rgba(theme.get().text.muted), move |c| el.set_placeholder_color(c));
            View::Node(el)
        })
    };

    let header = {
        let props = props.clone();
        let search_input = search_input.clone();
        let search_in_title = search_in_title.clone();
        move || {
            // Rows reserve a gutter, so the header and footer line up with the list's inset.
            let el = box_();
            el.set_padding(Edge::Left, point(DIALOG_LIST_INSET - DIALOG_INSET));
            el.set_padding(Edge::Right, point(DIALOG_LIST_INSET - DIALOG_INSET));
            let fallback_props = props.clone();
            let fallback_search = search_input.clone();
            let title_search = search_input.clone();
            let on_cancel = props.on_cancel.clone();
            insert(
                el,
                show(
                    move || search_in_title(),
                    move || {
                        let search = title_search.clone();
                        let on_cancel = on_cancel.clone();
                        component(move || {
                            dialog_header(HeaderProps {
                                title_view: Some(Rc::new(move || search())),
                                gap: Some(2.0),
                                on_close: on_cancel.clone(),
                                ..HeaderProps::default()
                            })
                        })
                    },
                    Some(Rc::new(move || {
                        let props = fallback_props.clone();
                        let search = fallback_search.clone();
                        let header = component({
                            let props = props.clone();
                            move || {
                                dialog_header(HeaderProps {
                                    title: Some(props.title.clone()),
                                    title_view: props.title_view.clone(),
                                    description: props.description.clone(),
                                    on_close: props.on_cancel.clone(),
                                    ..HeaderProps::default()
                                })
                            }
                        });
                        let filter_row = if props.render_filter {
                            let row = box_();
                            row.set_padding(Edge::Top, point(1.0));
                            insert(row, search(), At::Only);
                            View::Node(row)
                        } else {
                            View::Empty
                        };
                        View::list([header, filter_row])
                    })),
                ),
                At::Only,
            );
            View::Node(el)
        }
    };

    let footer = {
        let props = props.clone();
        let footer_actions = footer_actions.clone();
        FooterProps {
            inset: Some(DIALOG_LIST_INSET - DIALOG_INSET),
            actions: Some(Rc::new(move || {
                let all = footer_actions();
                let pick = |group: Group| all.iter().filter(|(_, g, _)| *g == group).map(|(_, _, a)| a.clone()).collect();
                FooterActions { secondary: pick(Group::Secondary), primary: pick(Group::Primary) }
            })),
            children: props.footer.clone(),
        }
    };

    let legacy_footer: Rc<dyn Fn() -> View> = {
        let props = props.clone();
        let footer_actions = footer_actions.clone();
        Rc::new(move || {
            let props = props.clone();
            let footer_actions = footer_actions.clone();
            let has_footer = props.footer.is_some();
            let visible_count = visible_count.clone();
            show(
                move || has_footer || visible_count() > 0,
                move || {
                    let footer_actions = footer_actions.clone();
                    let children = props.footer.clone();
                    component(move || {
                        dialog_footer(FooterProps {
                            inset: Some(DIALOG_LIST_INSET - DIALOG_INSET),
                            actions: Some(Rc::new(move || {
                                let all = footer_actions();
                                FooterActions {
                                    secondary: all.iter().filter(|(s, _, _)| *s != Some(Side::Right)).map(|(_, _, a)| a.clone()).collect(),
                                    primary: all.iter().filter(|(s, _, _)| *s == Some(Side::Right)).map(|(_, _, a)| a.clone()).collect(),
                                }
                            })),
                            children: children.clone(),
                        })
                    })
                },
                Some(Rc::new(|| {
                    let el = box_();
                    el.set_flex_shrink(Some(0.0));
                    View::Node(el)
                })),
            )
        })
    };

    let body = {
        let props = props.clone();
        let new_designs = new_designs.clone();
        let scroll = scroll.clone();
        let update_more_below = update_more_below.clone();
        let move_to = move_to.clone();
        let locked = locked.clone();
        let dialog = dialog.clone();
        let current = current.clone();
        move || {
            dialog_bleed(move || {
                let fallback = {
                    let props = props.clone();
                    Rc::new(move || {
                        let props = props.clone();
                        let render_filter = props.render_filter;
                        let no_match = props.no_match_view.clone();
                        show(
                            move || render_filter && filter.with(|f| !f.is_empty()),
                            move || match &no_match {
                                Some(view) => view(),
                                None => muted_line("No results found"),
                            },
                            Some(Rc::new(move || match &props.empty_view {
                                Some(view) => view(),
                                None => muted_line("No items available"),
                            })),
                        )
                    })
                };
                show(
                    move || grouped.with(|g| !g.is_empty()),
                    move || {
                        let s = scrollbox();
                        scroll.set(Some(s));
                        {
                            let new_designs = new_designs.clone();
                            bind(move || if new_designs() { 0.0 } else { 1.0 }, move |p| {
                                s.set_padding(Edge::Left, point(p));
                                s.set_padding(Edge::Right, point(p));
                            });
                        }
                        s.set_scrollbar_options(None, &ScrollBarOptions { visible: Some(Some(false)), ..Default::default() });
                        bind_always(move || scroll_acceleration.get().0, move |a| s.set_scroll_acceleration(a));
                        {
                            let update = update_more_below.clone();
                            s.on_scroll_change(true, Rc::new(move |_| update()));
                            let update = update_more_below.clone();
                            s.on_content_resize(Rc::new(move |_| update()));
                            let update = update_more_below.clone();
                            s.on_viewport_resize(Rc::new(move |_| update()));
                        }
                        bind(move || height.get() as f32, move |h| s.set_max_height(point(h)));
                        let move_to = move_to.clone();
                        let locked = locked.clone();
                        let dialog = dialog.clone();
                        let props = props.clone();
                        let current = current.clone();
                        let list_designs = new_designs.clone();
                        insert(
                            s,
                            index(
                                move || grouped.get(),
                                move |group, position| {
                                    let category = create_memo(move || group.with(|(c, _)| c.clone()));
                                    let header = show(
                                        move || category.with(|c| !c.is_empty()),
                                        move || select_category(category, position == 0),
                                        None,
                                    );
                                    let move_to = move_to.clone();
                                    let locked = locked.clone();
                                    let dialog = dialog.clone();
                                    let props = props.clone();
                                    let current = current.clone();
                                    let new_designs = list_designs.clone();
                                    let rows = index(
                                        move || group.with(|(_, options)| options.clone()),
                                        move |option, _| {
                                            option_row(
                                                option,
                                                category,
                                                OptionRowContext {
                                                    selected,
                                                    flat,
                                                    flatten,
                                                    selected_index,
                                                    move_to: move_to.clone(),
                                                    locked: locked.clone(),
                                                    dialog: dialog.clone(),
                                                    on_select: props.on_select.clone(),
                                                    current: current.clone(),
                                                    details_width,
                                                    new_designs: Rc::new(new_designs.clone()),
                                                },
                                            )
                                        },
                                        None,
                                    );
                                    View::list([header, rows])
                                },
                                None,
                            ),
                            At::Only,
                        );
                        let overflow = show(
                            ellipsis,
                            move || {
                                let el = box_();
                                el.set_padding(Edge::Left, point(DIALOG_LIST_INSET));
                                el.set_flex_shrink(Some(0.0));
                                let t = text();
                                bind_always(move || rgba(theme.get().text.muted), move |c| t.set_fg(Some(c)));
                                insert_node(t, text_node_from_string("…"), None);
                                insert_node(el, t, None);
                                View::Node(el)
                            },
                            None,
                        );
                        // Reserved whenever any option has a detail, so the dialog does not resize as the selection
                        // moves.
                        let detail = {
                            let new_designs = new_designs.clone();
                            show(
                                move || new_designs() && flat.with(|f| f.iter().any(|o| o.detail.is_some())),
                                move || {
                                    let el = box_();
                                    el.set_padding(Edge::Top, point(1.0));
                                    el.set_padding(Edge::Left, point(DIALOG_LIST_INSET));
                                    el.set_padding(Edge::Right, point(DIALOG_LIST_INSET));
                                    el.set_flex_shrink(Some(0.0));
                                    let view = create_memo_equals(
                                        move || match selected.get().and_then(|o| o.detail) {
                                            Some(Detail::View(render)) => Some(DetailView(render)),
                                            _ => None,
                                        },
                                        Some(|a: &Option<DetailView>, b: &Option<DetailView>| a == b),
                                    );
                                    insert(
                                        el,
                                        show_keyed(
                                            move || view.get(),
                                            |render| (render.0)(),
                                            Some(Rc::new(move || {
                                                let t = text();
                                                bind_always(
                                                    move || rgba(theme.get().surface(SurfaceName::DialogFooter).text.muted),
                                                    move |c| t.set_fg(Some(c)),
                                                );
                                                t.set_wrap_mode(WrapMode::Word);
                                                insert(
                                                    t,
                                                    View::dynamic(move || {
                                                        let text = match selected.get().and_then(|o| o.detail) {
                                                            Some(Detail::Text(text)) if !text.is_empty() => text,
                                                            _ => " ".to_string(),
                                                        };
                                                        View::from(text)
                                                    }),
                                                    At::Only,
                                                );
                                                View::Node(t)
                                            })),
                                        ),
                                        At::Only,
                                    );
                                    View::Node(el)
                                },
                                None,
                            )
                        };
                        View::list([View::Node(s), overflow, detail])
                    },
                    Some(fallback),
                )
            })
        }
    };

    let footer_gap = props.footer_gap;
    dialog_layout(LayoutProps {
        id: None,
        header: Box::new(header),
        footer: Some(footer),
        legacy_footer: Some(legacy_footer),
        inset: None,
        footer_gap: Some(Rc::new(move || footer_gap)),
        children: Box::new(body),
    })
}

/// A scroll acceleration value, compared by identity.
#[derive(Clone)]
struct Accel(Rc<dyn opencode_tui_core::renderables::scrollbox::ScrollAcceleration>);

#[derive(Clone)]
struct DetailView(Rc<dyn Fn() -> View>);

impl PartialEq for DetailView {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

fn muted_line(label: &'static str) -> View {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let el = box_();
    el.set_padding(Edge::Left, point(DIALOG_LIST_INSET));
    el.set_padding(Edge::Right, point(DIALOG_LIST_INSET));
    let t = text();
    bind_always(move || rgba(theme.get().text.muted), move |c| t.set_fg(Some(c)));
    insert_node(t, text_node_from_string(label), None);
    insert_node(el, t, None);
    View::Node(el)
}

/// `DialogSelectChoice`: the trailing value of a row that can be changed, under the "new-designs" experiment.
pub fn dialog_select_choice(value: impl Fn() -> String + 'static, active: impl Fn() -> bool + 'static, open: bool) -> View {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let active = Rc::new(active);
    let el = span();
    {
        let active = active.clone();
        bind_always(
            move || {
                let t = theme.get();
                if active() { rgba(t.text.action.primary.focused) } else { rgba(t.text.muted) }
            },
            move |c| el.set_span_style(Some(c), None, 0, 0),
        );
    }
    insert(
        el,
        View::dynamic(move || {
            let value = value();
            View::from(if !active() {
                format!("  {value}  ")
            } else if open {
                format!("  {value} ↵")
            } else {
                format!("‹ {value} ›")
            })
        }),
        At::Only,
    );
    View::Node(el)
}

/// `DialogSelectCategory`.
fn select_category(title: Memo<String>, first: bool) -> View {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let new_designs = use_new_designs();
    let el = box_();
    el.set_padding(Edge::Top, point(if first { 0.0 } else { 1.0 }));
    {
        let new_designs = new_designs.clone();
        bind(move || DIALOG_LIST_INSET - if new_designs() { 0.0 } else { 1.0 }, move |p| el.set_padding(Edge::Left, point(p)));
    }
    let t = text();
    bind_always(
        move || {
            let th = theme.get();
            if new_designs() { rgba(th.text.muted) } else { accent_200(&th) }
        },
        move |c| t.set_fg(Some(c)),
    );
    t.set_attributes(Some(text_attributes::BOLD));
    insert(t, View::dynamic(move || View::from(title.get())), At::Only);
    insert_node(el, t, None);
    View::Node(el)
}

fn accent_200(theme: &ResolvedTheme) -> RGBA {
    theme.hue.get("accent").and_then(|scale| scale.step(200)).map(rgba).unwrap_or(rgba(theme.text.base))
}

struct OptionRowContext<T: 'static> {
    selected: Memo<Option<SelectOption<T>>>,
    flat: Memo<Vec<SelectOption<T>>>,
    flatten: Memo<bool>,
    selected_index: Signal<usize>,
    move_to: Rc<dyn Fn(usize, bool, bool)>,
    locked: Rc<dyn Fn() -> bool>,
    dialog: DialogContext,
    on_select: Option<Rc<dyn Fn(&SelectOption<T>)>>,
    current: Rc<dyn Fn() -> Option<T>>,
    details_width: Memo<i64>,
    new_designs: Rc<dyn Fn() -> bool>,
}

/// One option: its row and details, with the mouse moving and choosing.
fn option_row<T: Clone + PartialEq + 'static>(option: Signal<SelectOption<T>>, category: Memo<String>, ctx: OptionRowContext<T>) -> View {
    let OptionRowContext { selected, flat, flatten, selected_index, move_to, locked, dialog, on_select, current, details_width, new_designs } =
        ctx;
    let active = {
        let locked = locked.clone();
        create_memo(move || !locked() && selected.with(|s| s.as_ref().map(|s| &s.value) == Some(&option.with(|o| o.value.clone()))))
    };
    let is_current = create_memo(move || current().is_some_and(|c| option.with(|o| o.value == c)));
    let el = box_();
    el.set_flex_direction(FlexDirection::Column);
    el.set_position_type(false);
    {
        let locked = locked.clone();
        let move_to = move_to.clone();
        el.set_on_mouse(
            Some(MouseEventType::Move),
            Some(Rc::new(move |_| {
                if locked() {
                    return;
                }
                let value = option.with_untracked(|o| o.value.clone());
                let Some(index) = flat.with_untracked(|f| f.iter().position(|x| x.value == value)) else { return };
                if index == selected_index.get_untracked() {
                    return;
                }
                move_to(index, false, true);
            })),
        );
    }
    {
        let locked = locked.clone();
        el.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |_| {
                if locked() {
                    return;
                }
                let current = option.get_untracked();
                if let Some(select) = &current.on_select {
                    select(&dialog);
                }
                if let Some(select) = &on_select {
                    select(&current);
                }
            })),
        );
    }
    {
        let locked = locked.clone();
        let move_to = move_to.clone();
        el.set_on_mouse(
            Some(MouseEventType::Down),
            Some(Rc::new(move |_| {
                if locked() {
                    return;
                }
                let value = option.with_untracked(|o| o.value.clone());
                let Some(index) = flat.with_untracked(|f| f.iter().position(|x| x.value == value)) else { return };
                move_to(index, false, true);
            })),
        );
    }
    let footer = create_memo(move || {
        option.with(|o| {
            if flatten.get() {
                o.search_footer.clone().or_else(|| o.category.clone().map(Footer::Text)).or_else(|| o.footer.clone())
            } else {
                o.footer.clone()
            }
        })
    });
    let description = create_memo(move || option.with(|o| o.description.clone().filter(|d| *d != category.get())));
    insert(
        el,
        component(move || {
            select_row(RowProps { option, footer, description, active, current: is_current, new_designs: new_designs.clone() })
        }),
        At::End,
    );
    let theme = use_theme().surface(SurfaceName::Dialog);
    insert(
        el,
        index(
            move || option.with(|o| o.details.clone()),
            move |detail, _| {
                let new_designs = use_new_designs();
                let row = box_();
                {
                    let new_designs = new_designs.clone();
                    bind(move || DIALOG_LIST_INSET - if new_designs() { 0.0 } else { 1.0 }, move |p| {
                        row.set_padding(Edge::Left, point(p));
                        row.set_padding(Edge::Right, point(p));
                    });
                }
                let t = text();
                bind_always(move || rgba(theme.get().text.muted), move |c| t.set_fg(Some(c)));
                t.set_wrap_mode(WrapMode::None);
                insert(
                    t,
                    View::dynamic(move || View::from(locale::truncate_middle(&detail.get(), details_width.get().max(1) as usize))),
                    At::Only,
                );
                insert_node(row, t, None);
                View::Node(row)
            },
            None,
        ),
        At::End,
    );
    View::Node(el)
}

struct RowProps<T: 'static> {
    option: Signal<SelectOption<T>>,
    footer: Memo<Option<Footer>>,
    description: Memo<Option<String>>,
    active: Memo<bool>,
    current: Memo<bool>,
    new_designs: Rc<dyn Fn() -> bool>,
}

/// `DialogSelectRow`.
fn select_row<T: Clone + PartialEq + 'static>(props: RowProps<T>) -> View {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let RowProps { option, footer, description, active, current, new_designs } = props;
    let text_color = {
        let new_designs = new_designs.clone();
        create_memo_equals(
            move || {
                let t = theme.get();
                if active.get() {
                    return option.with(|o| o.fg).unwrap_or(rgba(t.text.action.primary.focused));
                }
                if current.get() && !new_designs() {
                    return rgba(t.text.formfield.selected);
                }
                rgba(t.text.base)
            },
            Some(|a: &RGBA, b: &RGBA| a == b),
        )
    };
    // Secondary text (description and footer).
    let secondary = {
        let new_designs = new_designs.clone();
        move || {
            let t = theme.get();
            if !new_designs() {
                return if active.get() { text_color.get() } else { rgba(t.text.muted) };
            }
            if active.get() { rgba(t.text.action.secondary.focused) } else { rgba(t.text.muted) }
        }
    };
    let secondary = Rc::new(secondary);
    let row = {
        let new_designs = new_designs.clone();
        let secondary = secondary.clone();
        move || {
            // A marker or gutter takes its cell plus the row gap out of the left padding, so titles line up with
            // unmarked rows: scroll pad (1) + marker (1) + gap (1).
            let el = box_();
            el.set_flex_direction(FlexDirection::Row);
            {
                let new_designs = new_designs.clone();
                bind(move || new_designs(), move |n| el.set_flex_grow(if n { Some(1.0) } else { None }));
            }
            bind(
                move || {
                    if current.get() || option.with(|o| o.gutter.is_some()) { DIALOG_LIST_INSET - 3.0 } else { DIALOG_LIST_INSET - 1.0 }
                },
                move |p| el.set_padding(Edge::Left, point(p)),
            );
            {
                let new_designs = new_designs.clone();
                bind(
                    move || DIALOG_LIST_INSET - if new_designs() { 0.0 } else { 1.0 },
                    move |p| el.set_padding(Edge::Right, point(p)),
                );
            }
            el.set_gap(2, point(1.0));
            bind_always(
                move || {
                    if active.get() {
                        option.with(|o| o.bg).unwrap_or(rgba(theme.get().background.action.primary.focused))
                    } else {
                        RGBA::from_ints(0.0, 0.0, 0.0, 0.0)
                    }
                },
                move |c| el.set_background_color(Some(c)),
            );
            let margin = show_value(
                move || if current.get() { None } else { option.with(|o| o.margin.clone().map(DetailView)) },
                move |margin| {
                    let el = box_();
                    el.set_position_type(true);
                    el.set_positions(Some(point(DIALOG_LIST_INSET - 3.0)), None, None, None);
                    el.set_flex_shrink(Some(0.0));
                    insert(el, View::dynamic(move || margin.get().map(|m| (m.0)()).unwrap_or_default()), At::Only);
                    View::Node(el)
                },
                None,
            );
            let marker = {
                let new_designs = new_designs.clone();
                show(
                    move || current.get() && option.with(|o| o.gutter.is_none()),
                    move || {
                        let t = text();
                        t.set_flex_shrink(Some(0.0));
                        let new_designs = new_designs.clone();
                        let fg_designs = new_designs.clone();
                        bind_always(
                            move || if fg_designs() { rgba(theme.get().text.formfield.selected) } else { text_color.get() },
                            move |c| t.set_fg(Some(c)),
                        );
                        insert(t, View::dynamic(move || View::from(if new_designs() { "▪" } else { "●" })), At::Only);
                        View::Node(t)
                    },
                    None,
                )
            };
            let gutter = show_value(
                move || option.with(|o| o.gutter.clone().map(GutterView)),
                move |gutter| {
                    let el = box_();
                    el.set_flex_shrink(Some(0.0));
                    insert(el, View::dynamic(move || gutter.get().map(|g| (g.0)(text_color.get())).unwrap_or_default()), At::Only);
                    View::Node(el)
                },
                None,
            );
            let title = text();
            title.set_flex_grow(Some(1.0));
            bind_always(move || text_color.get(), move |c| title.set_fg(Some(c)));
            {
                let new_designs = new_designs.clone();
                bind(
                    move || if active.get() && !new_designs() { Some(text_attributes::BOLD) } else { None },
                    move |a| title.set_attributes(a),
                );
            }
            title.set_overflow(Overflow::Hidden);
            title.set_wrap_mode(WrapMode::None);
            title.set_padding(Edge::Left, point(3.0));
            insert(
                title,
                View::dynamic(move || {
                    option.with(|o| match &o.title_view {
                        Some(view) => view(),
                        None => View::from(match o.truncate_title {
                            TruncateTitle::None => o.title.clone(),
                            TruncateTitle::Left => locale::truncate_left(&o.title, o.title_width.unwrap_or(61)),
                            TruncateTitle::Right => locale::truncate(&o.title, o.title_width.unwrap_or(61)),
                        }),
                    })
                }),
                At::End,
            );
            {
                let secondary = secondary.clone();
                insert(
                    title,
                    show(
                        move || description.with(|d| d.as_ref().is_some_and(|d| !d.is_empty())),
                        move || {
                            let s = span();
                            let secondary = secondary.clone();
                            bind_always(move || secondary(), move |c| s.set_span_style(Some(c), None, 0, 0));
                            insert(s, View::dynamic(move || View::from(format!(" {}", description.get().unwrap_or_default()))), At::Only);
                            View::Node(s)
                        },
                        None,
                    ),
                    At::End,
                );
            }
            let trailing = {
                let secondary = secondary.clone();
                show(
                    move || footer.with(|f| f.as_ref().is_some_and(|f| !matches!(f, Footer::Text(t) if t.is_empty()))),
                    move || {
                        let el = box_();
                        el.set_flex_shrink(Some(0.0));
                        bind(
                            move || option.with(|o| o.footer_overhang.filter(|v| *v != 0.0).map(|v| -v)),
                            move |m| el.set_margin(Edge::Right, m.map(Value::Point)),
                        );
                        let t = text();
                        let secondary = secondary.clone();
                        bind_always(
                            move || {
                                let color = option.with(|o| o.footer_color);
                                if active.get() || color.is_none() { secondary() } else { color.unwrap() }
                            },
                            move |c| t.set_fg(Some(c)),
                        );
                        insert(
                            t,
                            View::dynamic(move || match footer.get() {
                                Some(Footer::Text(text)) => View::from(text),
                                Some(Footer::View(view)) => view(),
                                None => View::Empty,
                            }),
                            At::Only,
                        );
                        insert_node(el, t, None);
                        View::Node(el)
                    },
                    None,
                )
            };
            insert(el, margin, At::End);
            insert(el, marker, At::End);
            insert(el, gutter, At::End);
            insert_node(el, title, None);
            insert(el, trailing, At::End);
            View::Node(el)
        }
    };
    let fallback_row = row.clone();
    show(
        move || new_designs(),
        move || {
            let el = box_();
            el.set_flex_direction(FlexDirection::Row);
            let bar = text();
            bar.set_width(point(1.0));
            bar.set_flex_shrink(Some(0.0));
            bind_always(move || rgba(theme.get().text.formfield.focused), move |c| bar.set_fg(Some(c)));
            insert(bar, View::dynamic(move || View::from(if active.get() { "┃" } else { " " })), At::Only);
            insert_node(el, bar, None);
            insert(el, row(), At::End);
            View::Node(el)
        },
        Some(Rc::new(fallback_row)),
    )
}

#[derive(Clone)]
struct GutterView(Rc<dyn Fn(RGBA) -> View>);

impl PartialEq for GutterView {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
