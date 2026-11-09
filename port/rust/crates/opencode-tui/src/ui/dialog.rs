//! `ui/dialog.tsx`: the dialog stack (`DialogProvider`, `useDialog`: replace, clear, the modal input mode, escape
//! and ctrl+c, refocusing what had focus), the backdrop and panel (`Dialog`), and the layout pieces dialogs are
//! built from (`DialogHeader`, `DialogLayout`, `DialogBleed`, `DialogFooter`).
//!
//! TODO(port-tui): the renderer has no text selection yet, so `getSelection()` is always empty (a click on the
//! backdrop always dismisses; escape never clears a selection first) and the provider's copy-on-select handlers
//! are not wired.
use std::cell::Cell;
use std::rc::Rc;

use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{self, Align, Edge, Element, FlexDirection, Justify, MouseEventType};

use crate::ctx::tui;
use crate::keymap::{Command, Layer, create_layer};
use crate::theme::SurfaceName;
use crate::ui::theme::{provide_surface, use_theme};
use crate::util::rgba;

/// `DialogSize`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DialogSize {
    #[default]
    Medium,
    Large,
    Xlarge,
}

pub const DIALOG_INSET: f32 = 3.0;
/// List rows reserve a 1-cell gutter for the ▌ bar / ● marker.
pub const DIALOG_LIST_INSET: f32 = DIALOG_INSET + 1.0;

/// `dialogWidth(size)`.
pub fn dialog_width(size: DialogSize) -> u32 {
    match size {
        DialogSize::Xlarge => 116,
        DialogSize::Large => 88,
        DialogSize::Medium => 60,
    }
}

/// `useNewDesigns()`: the "new-designs" experiment, tracked.
pub fn use_new_designs() -> impl Fn() -> bool + Clone + 'static {
    let config = tui().config_context;
    move || config.data().new_designs()
}

fn point(v: f32) -> Option<Value> {
    Some(Value::Point(v))
}

/// `<Dialog size centered onClose>`: the backdrop over the whole terminal and the panel.
pub fn dialog(size: impl Fn() -> DialogSize + 'static, centered: impl Fn() -> bool + 'static, on_close: Rc<dyn Fn()>, children: impl FnOnce() -> View) -> View {
    let dimensions = tui().dimensions;
    let theme = use_theme().surface(SurfaceName::Dialog);
    let dismiss = Rc::new(Cell::new(false));
    // <ThemeContextProvider context="dialog">
    provide_surface(Some(SurfaceName::Dialog));
    let backdrop = box_();
    let panel = box_();
    insert_node(backdrop, panel, None);
    {
        let dismiss = dismiss.clone();
        // TODO(port-tui): `!!renderer.getSelection()` once the renderer has a selection.
        backdrop.set_on_mouse(Some(MouseEventType::Down), Some(Rc::new(move |_| dismiss.set(false))));
    }
    {
        let dismiss = dismiss.clone();
        backdrop.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |_| {
                if dismiss.get() {
                    dismiss.set(false);
                    return;
                }
                on_close();
            })),
        );
    }
    bind(move || dimensions.get(), move |(w, h)| {
        backdrop.set_width(point(w as f32));
        backdrop.set_height(point(h as f32));
    });
    backdrop.set_align_items(Align::Center);
    let centered = Rc::new(centered);
    {
        let centered = centered.clone();
        bind(move || centered(), move |c| backdrop.set_justify_content(if c { Justify::Center } else { Justify::FlexStart }));
    }
    backdrop.set_position_type(true);
    backdrop.set_z_index(3000);
    {
        let centered = centered.clone();
        bind(
            move || if centered() { 0.0 } else { dimensions.get().1 as f32 / 4.0 },
            move |p| backdrop.set_padding(Edge::Top, point(p)),
        );
    }
    backdrop.set_positions(Some(point(0.0)), Some(point(0.0)), None, None);
    backdrop.set_background_color(Some(RGBA::from_ints(0.0, 0.0, 0.0, 150.0)));
    {
        let dismiss = dismiss.clone();
        panel.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |event| {
                // A selection release must bubble up to the copy-on-select handler; the backdrop's dismiss flag keeps
                // it from closing the dialog. TODO(port-tui): the renderer's selection.
                dismiss.set(false);
                event.stop_propagation();
            })),
        );
    }
    bind(move || dialog_width(size()) as f32, move |w| panel.set_width(point(w)));
    bind(move || dimensions.get().0 as f32 - 2.0, move |w| panel.set_max_width(point(w)));
    bind_always(move || rgba(theme.get().background.base), move |c| panel.set_background_color(Some(c)));
    panel.set_padding(Edge::Top, point(1.0));
    insert(panel, children(), At::Only);
    View::Node(backdrop)
}

/// `DialogHeader` props.
#[derive(Default)]
pub struct HeaderProps {
    pub title: Option<Rc<dyn Fn() -> String>>,
    pub title_view: Option<Rc<dyn Fn() -> View>>,
    pub close_label: Option<String>,
    pub on_close: Option<Rc<dyn Fn()>>,
    pub gap: Option<f32>,
    /// A muted line below the title, under the "new-designs" experiment.
    pub description: Option<Rc<dyn Fn() -> Option<String>>>,
    pub children: Option<Rc<dyn Fn() -> View>>,
}

/// `<DialogHeader>`.
pub fn dialog_header(props: HeaderProps) -> View {
    let dialog = use_dialog();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let new_designs = use_new_designs();
    let props = Rc::new(props);
    let row = {
        let props = props.clone();
        let new_designs = new_designs.clone();
        let dialog = dialog.clone();
        move || {
            let el = box_();
            el.set_flex_direction(FlexDirection::Row);
            el.set_justify_content(Justify::SpaceBetween);
            if let Some(gap) = props.gap {
                el.set_gap(2, point(gap));
            }
            let title_part = match &props.title_view {
                Some(view) => view(),
                None => {
                    let title = text();
                    let new_designs = new_designs.clone();
                    bind(move || if new_designs() { None } else { Some(text_attributes::BOLD) }, move |a| title.set_attributes(a));
                    bind_always(move || rgba(theme.get().text.base), move |c| title.set_fg(Some(c)));
                    if let Some(value) = props.title.clone() {
                        insert(title, View::dynamic(move || View::from(value())), At::Only);
                    }
                    View::Node(title)
                }
            };
            let close = text();
            bind_always(move || rgba(theme.get().text.muted), move |c| close.set_fg(Some(c)));
            close.set_flex_shrink(Some(0.0));
            {
                let on_close = props.on_close.clone();
                let dialog = dialog.clone();
                close.set_on_mouse(
                    Some(MouseEventType::Up),
                    Some(Rc::new(move |_| match &on_close {
                        Some(on_close) => on_close(),
                        None => dialog.clear(),
                    })),
                );
            }
            insert_node(close, text_node_from_string(props.close_label.as_deref().unwrap_or("esc")), None);
            insert(el, title_part, At::End);
            if let Some(children) = &props.children {
                insert(el, children(), At::End);
            }
            insert_node(el, close, None);
            View::Node(el)
        }
    };
    let description = props.description.clone();
    let has_description = {
        let description = description.clone();
        move || description.as_ref().and_then(|d| d()).is_some_and(|d| !d.is_empty())
    };
    let row_fallback = row.clone();
    show(
        move || new_designs() && has_description(),
        move || {
            let el = box_();
            insert(el, row(), At::End);
            let line = text();
            bind_always(move || rgba(theme.get().text.muted), move |c| line.set_fg(Some(c)));
            line.set_wrap_mode(WrapMode::Word);
            let description = description.clone();
            insert(line, View::dynamic(move || View::from(description.as_ref().and_then(|d| d()).unwrap_or_default())), At::Only);
            insert_node(el, line, None);
            View::Node(el)
        },
        Some(Rc::new(row_fallback)),
    )
}

/// `DialogFooterAction`.
#[derive(Clone, Default)]
pub struct FooterAction {
    pub title: String,
    /// A literal key label, e.g. "enter".
    pub key: Option<String>,
    /// A keymap command whose first shortcut shows when `key` is not set; without a shortcut the action hides.
    pub command: Option<String>,
    pub on_trigger: Option<Rc<dyn Fn()>>,
    pub disabled: bool,
    /// Overrides the title and key color.
    pub color: Option<RGBA>,
}

impl PartialEq for FooterAction {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title
            && self.key == other.key
            && self.command == other.command
            && self.disabled == other.disabled
            && self.color == other.color
            && match (&self.on_trigger, &other.on_trigger) {
                (None, None) => true,
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                _ => false,
            }
    }
}

/// `DialogFooterActions`: `secondary` on the left (dialog-level), `primary` on the right (the selected item).
#[derive(Clone, Default, PartialEq)]
pub struct FooterActions {
    pub secondary: Vec<FooterAction>,
    pub primary: Vec<FooterAction>,
}

/// `DialogFooterProps`.
#[derive(Clone, Default)]
pub struct FooterProps {
    pub actions: Option<Rc<dyn Fn() -> FooterActions>>,
    pub inset: Option<f32>,
    pub children: Option<Rc<dyn Fn() -> View>>,
}

/// `<DialogLayout id header footer legacyFooter inset footerGap>`.
pub struct LayoutProps {
    pub id: Option<String>,
    pub header: Box<dyn FnOnce() -> View>,
    pub footer: Option<FooterProps>,
    /// The footer while the "new-designs" experiment is off; `None` is TS `undefined`.
    pub legacy_footer: Option<Rc<dyn Fn() -> View>>,
    pub inset: Option<f32>,
    pub footer_gap: Option<Rc<dyn Fn() -> Option<f32>>>,
    pub children: Box<dyn FnOnce() -> View>,
}

impl Default for LayoutProps {
    fn default() -> Self {
        LayoutProps {
            id: None,
            header: Box::new(|| View::Empty),
            footer: None,
            legacy_footer: None,
            inset: None,
            footer_gap: None,
            children: Box::new(|| View::Empty),
        }
    }
}

/// `<DialogLayout>`: header, body and an optional footer, one blank row between each and one at the bottom.
pub fn dialog_layout(props: LayoutProps) -> View {
    let inset = props.inset.unwrap_or(DIALOG_INSET);
    let new_designs = use_new_designs();
    let el = box_();
    if let Some(id) = &props.id {
        el.set_id(id);
    }
    el.set_padding(Edge::Left, point(inset));
    el.set_padding(Edge::Right, point(inset));
    el.set_padding(Edge::Bottom, point(1.0));
    el.set_gap(2, point(1.0));
    insert(el, (props.header)(), At::End);
    let body = box_();
    insert_node(el, body, None);
    {
        let new_designs = new_designs.clone();
        let has_footer = props.footer.is_some();
        let gap = props.footer_gap.clone();
        bind(
            move || {
                if new_designs() && has_footer {
                    gap.as_ref().and_then(|g| g()).unwrap_or(1.0) - 1.0
                } else {
                    0.0
                }
            },
            move |p| body.set_padding(Edge::Bottom, point(p)),
        );
    }
    insert(body, (props.children)(), At::Only);
    let footer = props.footer.clone();
    let legacy = props.legacy_footer.clone();
    let legacy_defined = legacy.is_some();
    insert(
        el,
        show(
            move || new_designs() || !legacy_defined,
            move || match footer.clone() {
                Some(footer) => component(move || dialog_footer(footer)),
                None => View::Empty,
            },
            legacy.map(|l| Rc::new(move || l()) as Children),
        ),
        At::End,
    );
    View::Node(el)
}

/// `<DialogBleed>`: a raised band across the layout's side padding.
pub fn dialog_bleed(children: impl FnOnce() -> View) -> View {
    let el = box_();
    el.set_margin(Edge::Left, point(-DIALOG_INSET));
    el.set_margin(Edge::Right, point(-DIALOG_INSET));
    insert(el, children(), At::Only);
    View::Node(el)
}

/// `<DialogFooter actions inset>{children}</DialogFooter>`.
pub fn dialog_footer(props: FooterProps) -> View {
    let dimensions = tui().dimensions;
    let shortcuts = crate::keymap::use_shortcuts();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let new_designs = use_new_designs();
    let narrow = move || dimensions.get().0 < 60;
    let resolve = move |actions: Vec<FooterAction>| -> Vec<FooterAction> {
        actions
            .into_iter()
            .filter_map(|action| {
                if action.key.is_some() || action.command.is_none() {
                    return Some(action);
                }
                let key = shortcuts.get(action.command.as_deref().unwrap_or_default())?;
                Some(FooterAction { key: Some(key), ..action })
            })
            .collect()
    };
    let actions = props.actions.clone();
    let secondary = {
        let actions = actions.clone();
        create_memo(move || resolve(actions.as_ref().map(|a| a().secondary).unwrap_or_default()))
    };
    let primary = {
        let actions = actions.clone();
        create_memo(move || resolve(actions.as_ref().map(|a| a().primary).unwrap_or_default()))
    };
    let has_content = props.children.is_some();
    let band = {
        let new_designs = new_designs.clone();
        create_memo(move || {
            new_designs() && (!secondary.with(Vec::is_empty) || !primary.with(Vec::is_empty) || has_content)
        })
    };
    // Hoverable actions carry one cell of padding on each side; the band gives that cell back.
    let band_inset = DIALOG_INSET + props.inset.unwrap_or(0.0) - 1.0;
    let inset = props.inset;
    let el = box_();
    bind(move || band.get(), move |b| {
        el.set_margin(Edge::Left, if b { point(-DIALOG_INSET) } else { None });
        el.set_margin(Edge::Right, if b { point(-DIALOG_INSET) } else { None });
        el.set_margin(Edge::Bottom, if b { point(-1.0) } else { None });
        let side = if b { point(band_inset) } else { inset.map(Value::Point) };
        el.set_padding(Edge::Left, side);
        el.set_padding(Edge::Right, side);
        el.set_padding(Edge::Top, if b { point(1.0) } else { None });
        el.set_padding(Edge::Bottom, if b { point(1.0) } else { None });
    });
    bind_always(
        move || if band.get() { Some(rgba(theme.get().background.raised.high)) } else { None },
        move |c| el.set_background_color(c),
    );
    el.set_flex_direction(FlexDirection::Row);
    el.set_justify_content(Justify::SpaceBetween);
    {
        let new_designs = new_designs.clone();
        bind(move || new_designs(), move |n| el.set_gap(2, if n { point(2.0) } else { None }));
    }
    el.set_flex_shrink(Some(0.0));
    let left = box_();
    insert_node(el, left, None);
    {
        let new_designs = new_designs.clone();
        bind(move || (narrow(), new_designs()), move |(narrow, new_designs)| {
            left.set_flex_direction(if narrow { FlexDirection::Column } else { FlexDirection::Row });
            left.set_gap(2, point(if narrow || new_designs { 0.0 } else { 2.0 }));
            left.set_gap(0, if new_designs && !narrow { point(1.0) } else { None });
            // Under new designs the primary group keeps its width and the secondary group wraps instead of
            // overlapping.
            left.set_flex_wrap(if new_designs { 1 } else { 0 });
            left.set_flex_shrink(if new_designs { Some(1.0) } else { Some(1.0) });
            left.set_min_width(if new_designs { point(0.0) } else { None });
        });
    }
    if let Some(children) = &props.children {
        insert(left, children(), At::End);
    }
    insert(
        left,
        for_each(move || secondary.get(), |a| a.title.clone(), |action, _| footer_item(action, false), None),
        At::End,
    );
    let right = box_();
    insert_node(el, right, None);
    right.set_flex_direction(FlexDirection::Row);
    {
        let new_designs = new_designs.clone();
        bind(move || new_designs(), move |n| {
            right.set_gap(2, point(if n { 1.0 } else { 2.0 }));
            right.set_flex_shrink(if n { Some(0.0) } else { Some(1.0) });
        });
    }
    insert(right, for_each(move || primary.get(), |a| a.title.clone(), |action, _| footer_item(action, true), None), At::Only);
    View::Node(el)
}

/// `DialogFooterItem`.
fn footer_item(action: Signal<FooterAction>, primary: bool) -> View {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let new_designs = use_new_designs();
    let (hovered, set_hovered) = create_signal(false);
    let trigger: Rc<dyn Fn()> = Rc::new(move || {
        let action = action.get_untracked();
        if action.disabled {
            return;
        }
        if let Some(run) = action.on_trigger {
            run();
        }
    });
    let label = move |key_color: fn(&crate::theme::ResolvedTheme) -> RGBA| {
        let line = text();
        let title = span();
        insert_node(line, title, None);
        bind_always(
            move || {
                let a = action.get();
                let t = theme.get();
                if a.disabled { rgba(t.text.action.primary.disabled) } else { a.color.unwrap_or(rgba(t.text.base)) }
            },
            move |c| title.set_span_style(Some(c), None, 0, 0),
        );
        insert(title, View::dynamic(move || View::from(action.get().title)), At::Only);
        insert(
            line,
            show(
                move || action.with(|a| a.key.as_ref().is_some_and(|k| !k.is_empty())),
                move || {
                    let key = span();
                    bind_always(
                        move || {
                            let a = action.get();
                            let t = theme.get();
                            if a.disabled { rgba(t.text.action.primary.disabled) } else { a.color.unwrap_or(key_color(&t)) }
                        },
                        move |c| key.set_span_style(Some(c), None, 0, 0),
                    );
                    insert(key, View::dynamic(move || View::from(format!(" {}", action.get().key.unwrap_or_default()))), At::Only);
                    View::Node(key)
                },
                None,
            ),
            At::End,
        );
        line
    };
    let muted: fn(&crate::theme::ResolvedTheme) -> RGBA = |t| rgba(t.text.muted);
    let fallback_trigger = trigger.clone();
    show(
        move || new_designs(),
        move || {
            let el = box_();
            el.set_padding(Edge::Left, point(1.0));
            el.set_padding(Edge::Right, point(1.0));
            el.set_flex_shrink(Some(0.0));
            bind_always(
                move || {
                    let t = theme.get();
                    let states = if primary { t.background.action.primary } else { t.background.action.secondary };
                    if hovered.get() && !action.with(|a| a.disabled) { Some(rgba(states.hovered)) } else { None }
                },
                move |c| el.set_background_color(c),
            );
            el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_hovered.set(true))));
            el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_hovered.set(false))));
            let trigger = trigger.clone();
            el.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| trigger())));
            insert_node(el, label(muted), None);
            View::Node(el)
        },
        Some(Rc::new(move || {
            let line = label(muted);
            let trigger = fallback_trigger.clone();
            line.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| trigger())));
            View::Node(line)
        })),
    )
}

/// An entry of the stack.
pub struct StackItem {
    pub element: Rc<dyn Fn() -> View>,
    pub on_close: Option<Rc<dyn Fn()>>,
    pub key: Option<&'static str>,
}

/// A stack entry, compared by identity (`<Show when={stack.at(-1)} keyed>`).
#[derive(Clone)]
pub struct Entry(pub Rc<StackItem>);

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// `useDialog()`.
#[derive(Clone)]
pub struct DialogContext {
    stack: Signal<Vec<Entry>>,
    size: Signal<DialogSize>,
    centered: Signal<bool>,
    focus: Rc<Cell<Option<Element>>>,
}

impl DialogContext {
    fn refocus(&self) {
        let stack = self.stack;
        let focus = self.focus.clone();
        renderer::set_timeout(1, move || {
            if stack.with_untracked(|s| !s.is_empty()) {
                return;
            }
            let Some(focus) = focus.get() else { return };
            if !focus.is_alive() || focus.is_destroyed() {
                return;
            }
            // Still in the tree?
            let mut cursor = Some(focus);
            let root = tree::root();
            while let Some(el) = cursor {
                if Some(el) == root {
                    focus.focus();
                    return;
                }
                cursor = el.parent();
            }
        });
    }

    /// Closes the top dialog (escape / ctrl+c).
    fn pop(&self) {
        let current = self.stack.with_untracked(|s| s.last().cloned());
        if let Some(on_close) = current.and_then(|c| c.0.on_close.clone()) {
            on_close();
        }
        self.stack.update(|s| {
            s.pop();
        });
        self.refocus();
    }

    /// `clear()`: closes every dialog.
    pub fn clear(&self) {
        let items = self.stack.get_untracked();
        for item in &items {
            if let Some(on_close) = &item.0.on_close {
                on_close();
            }
        }
        batch(|| {
            self.size.set(DialogSize::Medium);
            self.centered.set(false);
            self.stack.set(Vec::new());
        });
        self.refocus();
    }

    /// `replace(element, onClose, { key, size })`: shows `element` in place of every open dialog.
    pub fn replace(&self, element: impl Fn() -> View + 'static, on_close: Option<Rc<dyn Fn()>>, key: Option<&'static str>, size: Option<DialogSize>) {
        if self.stack.with_untracked(Vec::is_empty) {
            let focused = renderer::current_focused_renderable();
            self.focus.set(focused);
            if let Some(focused) = focused {
                focused.blur();
            }
        }
        let items = self.stack.get_untracked();
        for item in &items {
            if let Some(on_close) = &item.0.on_close {
                on_close();
            }
        }
        batch(|| {
            self.size.set(size.unwrap_or_default());
            self.centered.set(false);
            self.stack.set(vec![Entry(Rc::new(StackItem { element: Rc::new(element), on_close, key }))]);
        });
    }

    /// `replace(element)`.
    pub fn show(&self, element: impl Fn() -> View + 'static) {
        self.replace(element, None, None, None);
    }

    /// `stack.length` (tracked).
    pub fn len(&self) -> usize {
        self.stack.with(Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `size` (tracked).
    pub fn size(&self) -> DialogSize {
        self.size.get()
    }

    /// `centered` (tracked).
    pub fn centered(&self) -> bool {
        self.centered.get()
    }

    /// `key`: the top dialog's key (tracked).
    pub fn key(&self) -> Option<&'static str> {
        self.stack.with(|s| s.last().and_then(|e| e.0.key))
    }

    pub fn set_size(&self, size: DialogSize) {
        self.size.set(size);
    }

    pub fn set_centered(&self, centered: bool) {
        self.centered.set(centered);
    }
}

thread_local! {
    static DIALOG: Context<DialogContext> = create_context(None);
}

/// `useDialog()`.
pub fn use_dialog() -> DialogContext {
    use_context(DIALOG.with(|c| *c)).expect("useDialog must be used within a DialogProvider")
}

/// `init()` of `DialogProvider`, provided as `useDialog()` for what the current owner creates next.
pub fn provide() -> DialogContext {
    let value = DialogContext {
        stack: create_signal_with(Vec::new(), None).0,
        size: create_signal(DialogSize::Medium).0,
        centered: create_signal(false).0,
        focus: Rc::new(Cell::new(None)),
    };
    let stack = value.stack;
    create_effect(move || {
        if stack.with(Vec::is_empty) {
            return;
        }
        let pop = crate::keymap::mode_push("modal", None);
        let pop = Cell::new(Some(pop));
        on_cleanup(move || {
            if let Some(pop) = pop.take() {
                pop();
            }
        });
    });
    {
        let value = value.clone();
        create_layer(move || {
            let close = value.clone();
            let ctrl_c = value.clone();
            Layer {
                mode: Some("modal"),
                enabled: Some(crate::keymap::Enabled::Bool(value.stack.with(|s| !s.is_empty()))),
                commands: vec![
                    // TODO(port-tui): `renderer.getSelection()` clears a selection first.
                    Command::inline("escape", move || {
                        close.pop();
                        true
                    })
                    .title("Close dialog")
                    .group("Dialog"),
                    Command::inline("ctrl+c", move || {
                        if let Some(editor) = renderer::current_focused_renderable().filter(|e| e.editor_view().is_some()) {
                            if !editor.textarea_text().is_empty() {
                                if editor.is_input() {
                                    editor.set_input_value("");
                                } else {
                                    editor.set_textarea_text("");
                                }
                                return true;
                            }
                        }
                        ctrl_c.pop();
                        true
                    })
                    .title("Close dialog")
                    .group("Dialog"),
                ],
                ..Layer::default()
            }
        });
    }
    provide_context(DIALOG.with(|c| *c), value.clone());
    value
}

/// The provider's dialog layer, after its children: the top dialog over everything.
pub fn dialog_layer(value: DialogContext) -> View {
    let el = box_();
    el.set_position_type(true);
    el.set_z_index(3000);
    let stack = value.stack;
    let size = value.size;
    let centered = value.centered;
    insert(
        el,
        show(
            move || stack.with(|s| !s.is_empty()),
            move || {
                let close = value.clone();
                component(move || {
                    dialog(
                        move || size.get(),
                        move || centered.get(),
                        Rc::new(move || close.clear()),
                        move || {
                            // Each entry renders once: an element function inserted directly would re-run on a
                            // signal it reads at its root.
                            show_keyed(move || stack.with(|s| s.last().cloned()), |item| (item.0.element)(), None)
                        },
                    )
                })
            },
            None,
        ),
        At::Only,
    );
    View::Node(el)
}
