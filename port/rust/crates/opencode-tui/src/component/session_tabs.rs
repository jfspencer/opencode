//! `component/session-tabs.tsx` and `component/session-tabs-rail.tsx`: the horizontal tab strip and the vertical tab
//! rail. The strip's width allocation is `adaptiveSessionTabLayout` ([`crate::tabs_model`]); a tab's number/status
//! cell, title, close affordance and detail line are laid out as in TS.
//!
//! Animations are timing, so the motion values collapse to their targets: `animations: false` makes every
//! `createAnimatable` jump. The tab pulse renderables paint background cells only, so they are not ported here
//! (`TabPulse`'s text cells are the same); the strip's backgrounds and text colors are.
use std::rc::Rc;

use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, scrollbox, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::scrollbox::ScrollBarOptions;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection, Justify, MouseEventType};
use opencode_tui_sync::json::JsonExt;

use crate::ctx::tui;
use crate::data::{get_str};
use crate::tabs::{Attention, SessionTabs, TabStatus, tabs};
use crate::tabs_model::{
    NEW_SESSION_TAB_TITLE, SessionTab, TabLayout, adaptive_session_tab_layout, move_session_tab, session_tab_detail,
    session_tab_number_label, session_tab_overflow_width,
};
use crate::theme::ResolvedTheme;
use crate::util::{graphemes, rgba, take_width, tint};

/// `SESSION_TABS_COMPACT_BREAKPOINT` (`ui/layout.ts`).
const SESSION_TABS_COMPACT_BREAKPOINT: u32 = 12;
const SESSION_SIDEBAR_WIDTH: u32 = 42;
/// The add button renders as `" + "`, so the layout leaves it room.
const ADD_TAB_WIDTH: i64 = 3;

/// `TAB_SPINNERS.dots.frames[0]`.
fn spinner_frame() -> &'static str {
    crate::spinner::SPINNER_FRAMES[0]
}

/// The unread marker (`TAB_UNREAD_MARKERS["small-dot"]`).
const UNREAD_MARKER: &str = "•";

/// `<SessionTabs ...>`.
#[derive(Clone, Default)]
pub struct SessionTabsProps {
    pub animations: Option<bool>,
    pub indicators: Option<String>,
    pub orientation: Option<String>,
    pub width: Option<u32>,
}

/// `<SessionTabs>`: vertical when asked, horizontal otherwise.
pub fn session_tabs(props: SessionTabsProps) -> View {
    let config = tui().config_context.clone();
    let indicators = props.indicators.clone().unwrap_or_else(|| config.data_untracked().tabs.indicators.clone());
    let numbers = indicators == "numbers";
    if props.orientation.as_deref() == Some("vertical") {
        return component(move || vertical_session_tabs(props.clone(), numbers));
    }
    component(move || horizontal_session_tabs(props.clone(), numbers))
}

/// `activeID`: the pseudo new-session tab while it is promoted, else the viewed session.
fn active_id(controller: &SessionTabs) -> Option<String> {
    if controller.new_tab() {
        return Some("new".to_string());
    }
    controller.current()
}

fn statuses(controller: &SessionTabs, items: &[SessionTab]) -> Vec<TabStatus> {
    items
        .iter()
        .map(|tab| {
            if tab.session_id == "new" {
                TabStatus::default()
            } else {
                controller.status(&tab.session_id)
            }
        })
        .collect()
}

/// The indicator's label for a tab (`TabIndicator`'s `label()`).
fn indicator_label(status: &TabStatus, numbers: bool, number: &str, plus: bool) -> String {
    if numbers {
        return number.to_string();
    }
    match status.attention {
        Attention::Permission => return "!".to_string(),
        Attention::Question => return "?".to_string(),
        Attention::None => {}
    }
    if status.busy && status.attention == Attention::None {
        return spinner_frame().to_string();
    }
    if plus {
        return "+".to_string();
    }
    if status.unread.is_some() {
        return UNREAD_MARKER.to_string();
    }
    String::new()
}

fn number_width(items: usize) -> i64 {
    (items.to_string().len() as i64).max(2)
}

fn title_of(tab: &SessionTab) -> String {
    let data = tui().data.clone();
    data.store
        .session
        .get(&tab.session_id)
        .as_ref()
        .and_then(|session| session.str("title").map(str::to_string))
        .or_else(|| tab.title.clone())
        .unwrap_or_else(|| "Untitled session".to_string())
}

fn feedback_color(theme: &ResolvedTheme, status: &TabStatus) -> Option<opencode_tui_core::color::RGBA> {
    match status.attention {
        Attention::Permission | Attention::Question => Some(rgba(theme.hue.accent[200])),
        Attention::None => status.unread.filter(|u| *u == crate::tabs_model::Unread::Error).map(|_| rgba(theme.text.feedback.error.base)),
    }
}

fn unread_color(theme: &ResolvedTheme) -> opencode_tui_core::color::RGBA {
    rgba(theme.hue.accent[200])
}

fn active_number(theme: &ResolvedTheme) -> opencode_tui_core::color::RGBA {
    rgba(theme.hue.interactive[200])
}

fn idle_number(theme: &ResolvedTheme) -> opencode_tui_core::color::RGBA {
    tint(rgba(theme.text.formfield.base), rgba(theme.background.base), 0.55)
}

/// `background` of a horizontal tab: lifted when hovered or dragged, raised by its selection.
fn horizontal_background(theme: &ResolvedTheme, hovered: bool, dragged: bool, selected: bool) -> opencode_tui_core::color::RGBA {
    let lifted = (hovered || dragged) && !selected;
    let base = if lifted { theme.background.action.primary.hovered } else { theme.background.base };
    let raised = theme.decrease(theme.background.raised.base, 1.0);
    tint(rgba(base), rgba(raised), if dragged { 1.0 } else { if selected { 1.0 } else { 0.0 } })
}

fn horizontal_session_tabs(props: SessionTabsProps, numbers: bool) -> View {
    let controller = tabs();
    let t = tui();
    let dimensions = t.dimensions;
    let theme = t.theme;
    let animations = props.animations.unwrap_or_else(|| t.config_context.data_untracked().animations.unwrap_or(true));
    let hovered: Signal<Option<String>> = create_signal(None::<String>).0;
    let dragging: Signal<Option<String>> = create_signal(None::<String>).0;
    let add_hovered = create_signal(false).0;
    let preview: Signal<Option<(String, i64)>> = create_signal(None::<(String, i64)>).0;

    let ordered = {
        let controller = controller.clone();
        move || match preview.get() {
            Some((session_id, index)) => move_session_tab(&controller.list(), &session_id, index),
            None => controller.list(),
        }
    };
    let items: Rc<dyn Fn() -> Vec<SessionTab>> = {
        let controller = controller.clone();
        let ordered = ordered.clone();
        Rc::new(move || {
            let mut items = ordered();
            if controller.new_tab() {
                items.push(SessionTab::new("new", Some(NEW_SESSION_TAB_TITLE.to_string())));
            }
            items
        })
    };
    let show_plus = {
        let controller = controller.clone();
        move || !controller.new_tab()
    };
    let layout = {
        let controller = controller.clone();
        let items = items.clone();
        let show_plus = show_plus.clone();
        create_memo_with(move |previous: Option<TabLayout>| {
            let previous_start = previous.map_or(0, |layout| layout.start);
            adaptive_session_tab_layout(
                &items(),
                active_id(&controller).as_deref(),
                dimensions.get().0 as i64 - if show_plus() { ADD_TAB_WIDTH } else { 0 },
                previous_start,
            )
        })
    };
    // A membership change ends a stale preview.
    {
        let controller = controller.clone();
        create_effect(move || {
            let Some((session_id, index)) = preview.get() else { return };
            if dragging.get().is_some() {
                return;
            }
            let tabs = controller.list();
            match tabs.iter().position(|tab| tab.session_id == session_id) {
                None => preview.set(None),
                Some(found) if found == (index.min(tabs.len() as i64 - 1)) as usize => preview.set(None),
                _ => {}
            }
        });
    }
    let statuses_memo = {
        let controller = controller.clone();
        let items = items.clone();
        create_memo(move || statuses(&controller, &items()))
    };

    let strip = box_();
    strip.set_height(Some(Value::Point(1.0)));
    strip.set_flex_shrink(Some(0.0));
    strip.set_position_type(false);
    strip.set_flex_direction(FlexDirection::Row);
    strip.set_z_index(1);
    strip.set_on_mouse(
        Some(MouseEventType::Out),
        Some(Rc::new({
            let hovered = hovered;
            move |_| hovered.set(None)
        })),
    );
    {
        let controller = controller.clone();
        let hovered = hovered;
        let dragging = dragging;
        let preview = preview;
        strip.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |_| {
                let source = dragging.get();
                if let Some(source) = source {
                    dragging.set(None);
                    if let Some((id, index)) = preview.get() {
                        if id == source {
                            controller.move_tab(&id, index);
                        }
                    }
                    controller.select(&source);
                }
                hovered.set(None);
            })),
        );
    }

    // `‹N` and ` N›` markers.
    insert(
        strip,
        show(
            {
                let layout = layout.clone();
                move || layout.get().before > 0
            },
            {
                let layout = layout.clone();
                move || {
                    let layout = layout.get();
                    let marker = text();
                    marker.set_width(Some(Value::Point(session_tab_overflow_width(layout.before) as f32)));
                    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| marker.set_fg(Some(c)));
                    marker.set_selectable(false);
                    let label = format!("‹{}", layout.before);
                    insert(marker, View::from(label), At::Only);
                    View::Node(marker)
                }
            },
            None,
        ),
        At::End,
    );

    insert(
        strip,
        for_each(
            {
                let layout = layout.clone();
                move || layout.get().tabs
            },
            |tab: &SessionTab| tab.session_id.clone(),
            {
                let controller = controller.clone();
                let layout = layout.clone();
                let statuses_memo = statuses_memo.clone();
                let items = items.clone();
                let hovered = hovered;
                let dragging = dragging;
                move |tab: Signal<SessionTab>, index: Signal<usize>| {
                    horizontal_tab(
                        controller.clone(),
                        layout.clone(),
                        statuses_memo.clone(),
                        items.clone(),
                        hovered,
                        dragging,
                        theme,
                        animations,
                        numbers,
                        tab,
                        index,
                    )
                }
            },
            None,
        ),
        At::End,
    );

    insert(
        strip,
        show(
            {
                let layout = layout.clone();
                move || layout.get().after > 0
            },
            {
                let layout = layout.clone();
                move || {
                    let layout = layout.get();
                    let marker = text();
                    marker.set_width(Some(Value::Point(session_tab_overflow_width(layout.after) as f32)));
                    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| marker.set_fg(Some(c)));
                    marker.set_selectable(false);
                    let label = format!(" {}›", layout.after);
                    insert(marker, View::from(label), At::Only);
                    View::Node(marker)
                }
            },
            None,
        ),
        At::End,
    );

    // The add control: `" + "`, or the promoted new-session slot as a tab of its own.
    insert(
        strip,
        show(
            {
                let show_plus = show_plus.clone();
                move || show_plus()
            },
            {
                let controller = controller.clone();
                move || {
                    let el = text();
                    el.set_width(Some(Value::Point(ADD_TAB_WIDTH as f32)));
                    bind_always(
                        move || {
                            let theme = theme.get();
                            if add_hovered.get() {
                                rgba(theme.text.base)
                            } else {
                                rgba(theme.text.muted)
                            }
                        },
                        move |c| el.set_fg(Some(c)),
                    );
                    bind_always(
                        move || {
                            let theme = theme.get();
                            if add_hovered.get() {
                                Some(rgba(theme.background.action.primary.hovered))
                            } else {
                                None
                            }
                        },
                        move |c| el.set_bg(c),
                    );
                    el.set_selectable(false);
                    el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| add_hovered.set(true))));
                    el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| add_hovered.set(false))));
                    el.set_on_mouse(Some(MouseEventType::Down), Some(Rc::new({
                        let controller = controller.clone();
                        move |_| controller.add()
                    })));
                    insert(el, View::from(" + "), At::Only);
                    View::Node(el)
                }
            },
            None,
        ),
        At::End,
    );

    View::Node(strip)
}

#[allow(clippy::too_many_arguments)]
fn horizontal_tab(
    controller: SessionTabs,
    layout: Memo<TabLayout>,
    statuses: Memo<Vec<TabStatus>>,
    items: Rc<dyn Fn() -> Vec<SessionTab>>,
    hovered: Signal<Option<String>>,
    dragging: Signal<Option<String>>,
    theme: Signal<ResolvedTheme>,
    animations: bool,
    numbers: bool,
    tab: Signal<SessionTab>,
    index: Signal<usize>,
) -> View {
    let session_id = {
        let tab = tab;
        move || tab.get().session_id
    };
    let width = {
        let layout = layout.clone();
        let session_id = session_id.clone();
        move || {
            layout
                .get()
                .tabs
                .iter()
                .position(|tab| tab.session_id == session_id())
                .map_or(1, |position| layout.get().widths[position])
        }
    };
    let selected = {
        let controller = controller.clone();
        let session_id = session_id.clone();
        move || active_id(&controller).as_deref() == Some(session_id().as_str())
    };
    let status = {
        let statuses = statuses.clone();
        let session_id = session_id.clone();
        let items = items.clone();
        move || {
            items()
                .iter()
                .position(|item| item.session_id == session_id())
                .and_then(|position| statuses.get().get(position).cloned())
                .unwrap_or_default()
        }
    };
    let is_new = {
        let session_id = session_id.clone();
        move || session_id() == "new"
    };

    let el = box_();
    bind(width.clone(), move |width| el.set_width(Some(Value::Point(width as f32))));
    el.set_flex_shrink(Some(0.0));
    el.set_position_type(false);
    el.set_flex_direction(FlexDirection::Row);
    bind_always(
        {
            let hovered = hovered;
            let dragging = dragging;
            let selected = selected.clone();
            let session_id = session_id.clone();
            move || {
                let theme = theme.get();
                horizontal_background(
                    &theme,
                    hovered.get().as_deref() == Some(session_id().as_str()),
                    dragging.get().as_deref() == Some(session_id().as_str()),
                    selected(),
                )
            }
        },
        move |color| el.set_background_color(Some(color)),
    );
    {
        let hovered = hovered;
        let session_id = session_id.clone();
        el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| hovered.set(Some(session_id())))));
    }
    {
        let hovered = hovered;
        el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| hovered.set(None))));
    }

    let inner = box_();
    insert_node(el, inner, None);
    inner.set_z_index(1);
    inner.set_width(Some(Value::Percent(100.0)));
    inner.set_flex_direction(FlexDirection::Row);

    // The number/status cell: `numberWidth + 1` wide, right aligned with one padding cell.
    let number_width = {
        let items = items.clone();
        move || number_width(items().len())
    };
    let indicator = box_();
    insert_node(inner, indicator, None);
    bind(
        {
            let number_width = number_width.clone();
            move || number_width() + 1
        },
        move |width| indicator.set_width(Some(Value::Point(width as f32))),
    );
    indicator.set_flex_shrink(Some(0.0));
    indicator.set_flex_direction(FlexDirection::Row);
    indicator.set_justify_content(Justify::FlexEnd);
    indicator.set_padding(Edge::Right, Some(Value::Point(1.0)));
    let indicator_text = text();
    insert_node(indicator, indicator_text, None);
    indicator_text.set_selectable(false);
    bind(
        {
            let status = status.clone();
            let is_new = is_new.clone();
            let index = index;
            let numbers = numbers;
            move || {
                let number = session_tab_number_label(index.get());
                indicator_label(&status(), numbers, &number, is_new())
            }
        },
        move |label| indicator_text.set_content(&label),
    );
    bind_always(
        {
            let status = status.clone();
            let selected = selected.clone();
            let number_width = number_width.clone();
            let hovered = hovered;
            let session_id = session_id.clone();
            move || {
                let theme = theme.get();
                let status = status();
                let feedback = feedback_color(&theme, &status);
                let base = if hovered.get().as_deref() == Some(session_id().as_str()) && !selected() {
                    rgba(theme.text.base)
                } else {
                    tint(idle_number(&theme), tint(rgba(theme.text.base), rgba(theme.background.base), 0.25), if selected() { 1.0 } else { 0.0 })
                };
                if status.busy && !matches!(status.attention, Attention::Permission | Attention::Question) {
                    active_number(&theme)
                } else {
                    let _ = number_width;
                    feedback.unwrap_or_else(|| tint(base, unread_color(&theme), 0.0))
                }
            }
        },
        move |color| indicator_text.set_fg(Some(color)),
    );
    bind(
        {
            let selected = selected.clone();
            move || selected()
        },
        move |selected| indicator_text.set_attributes(if selected { Some(text_attributes::BOLD) } else { None }),
    );

    // The title: its right bound shifts left by two cells while hovered for the close mark.
    let resting_title_width = {
        let width = width.clone();
        let number_width = number_width.clone();
        move || (width() - 1 - number_width()).max(1)
    };
    let title_width = {
        let resting = resting_title_width.clone();
        let hovered = hovered;
        let session_id = session_id.clone();
        move || {
            if hovered.get().as_deref() == Some(session_id().as_str()) {
                (resting() - 2).max(1)
            } else {
                resting()
            }
        }
    };
    let title_el = text();
    insert_node(inner, title_el, None);
    title_el.set_wrap_mode(WrapMode::None);
    title_el.set_truncate(true);
    title_el.set_selectable(false);
    bind(title_width.clone(), move |width| title_el.set_width(Some(Value::Point(width as f32))));
    bind(
        {
            let title_width = title_width.clone();
            let tab = tab;
            move || take_width(&title_of(&tab.get()), title_width())
        },
        move |title| title_el.set_content(&title),
    );
    bind_always(
        {
            let selected = selected.clone();
            let hovered = hovered;
            let session_id = session_id.clone();
            move || {
                let theme = theme.get();
                if hovered.get().as_deref() == Some(session_id().as_str()) {
                    rgba(theme.text.base)
                } else {
                    tint(rgba(theme.text.muted), rgba(theme.text.base), if selected() { 1.0 } else { 0.0 })
                }
            }
        },
        move |color| title_el.set_fg(Some(color)),
    );
    bind(
        {
            let selected = selected.clone();
            move || selected()
        },
        move |selected| title_el.set_attributes(if selected { Some(text_attributes::BOLD) } else { None }),
    );

    // The close mark: an absolute cell at the right edge, one in from it, shown while hovered.
    let close_el = text();
    insert_node(inner, close_el, None);
    close_el.set_position_type(true);
    close_el.set_positions(None, None, Some(Some(Value::Point(1.0))), None);
    close_el.set_z_index(2);
    close_el.set_width(Some(Value::Point(1.0)));
    close_el.set_selectable(false);
    bind_always(
        move || {
            let theme = theme.get();
            tint(rgba(theme.text.muted), rgba(theme.text.base), 0.6)
        },
        move |color| close_el.set_fg(Some(color)),
    );
    bind(
        {
            let hovered = hovered;
            let session_id = session_id.clone();
            move || if hovered.get().as_deref() == Some(session_id().as_str()) { "✕".to_string() } else { String::new() }
        },
        move |label| close_el.set_content(&label),
    );
    {
        let hovered = hovered;
        let session_id = session_id.clone();
        close_el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| hovered.set(Some(session_id())))));
    }
    {
        let controller = controller.clone();
        let hovered = hovered;
        let session_id = session_id.clone();
        let is_new = is_new.clone();
        close_el.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |event| {
                if event.button == 2 || hovered.get().as_deref() != Some(session_id().as_str()) {
                    return;
                }
                event.stop_propagation();
                if is_new() {
                    controller.close(None);
                } else {
                    let id = session_id();
                    controller.close(Some(&id));
                }
            })),
        );
    }
    let _ = animations;
    View::Node(el)
}

// ---------------------------------------------------------------------------------------------------------------
// Vertical rail
// ---------------------------------------------------------------------------------------------------------------

fn vertical_session_tabs(props: SessionTabsProps, numbers: bool) -> View {
    let controller = tabs();
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let animations = props.animations.unwrap_or_else(|| t.config_context.data_untracked().animations.unwrap_or(true));
    let width = props.width.unwrap_or(SESSION_SIDEBAR_WIDTH);
    let compact = width < SESSION_TABS_COMPACT_BREAKPOINT;
    let hovered: Signal<Option<String>> = create_signal(None::<String>).0;
    let add_hovered = create_signal(false).0;
    let dragging: Signal<Option<String>> = create_signal(None::<String>).0;
    let preview: Signal<Option<(String, i64)>> = create_signal(None::<(String, i64)>).0;
    let scroll_top = create_signal(0.0f64).0;

    let ordered: Rc<dyn Fn() -> Vec<SessionTab>> = {
        let controller = controller.clone();
        Rc::new(move || match preview.get() {
            Some((session_id, index)) => move_session_tab(&controller.list(), &session_id, index),
            None => controller.list(),
        })
    };
    let items = ordered.clone();
    let active: Rc<dyn Fn() -> Option<String>> = {
        let controller = controller.clone();
        Rc::new(move || if controller.new_tab() { None } else { controller.current() })
    };
    let statuses_memo = {
        let controller = controller.clone();
        let items = items.clone();
        create_memo(move || statuses(&controller, &items()))
    };
    let below_highlighted: Rc<dyn Fn() -> bool> = {
        let items = items.clone();
        let active = active.clone();
        let hovered = hovered;
        let dragging = dragging;
        let controller = controller.clone();
        Rc::new(move || {
            let stride = if compact { 2.0 } else { 3.0 };
            let items = items();
            let index = (scroll_top.get() / stride).floor() as usize;
            match items.get(index) {
                Some(tab) => highlighted(active.as_ref(), hovered, dragging, &tab.session_id),
                None => controller.new_tab() || add_hovered.get(),
            }
        })
    };

    let rail = box_();
    rail.set_width(Some(Value::Point(width as f32)));
    rail.set_height(Some(Value::Percent(100.0)));
    rail.set_flex_shrink(Some(0.0));
    rail.set_flex_direction(FlexDirection::Column);
    rail.set_position_type(false);
    rail.set_padding(Edge::Top, Some(Value::Point(1.0)));
    bind_always(move || rgba(theme.with(|th| th.background.raised.base)), move |c| rail.set_background_color(Some(c)));
    rail.set_on_mouse(
        Some(MouseEventType::Out),
        Some(Rc::new({
            let hovered = hovered;
            move |_| hovered.set(None)
        })),
    );

    if compact {
        let controls = controller.clone();
        let below = below_highlighted.clone();
        insert(rail, component(move || rail_controls(&controls, width, below.clone(), theme)), At::End);
    }

    let scroll = scrollbox();
    insert_node(rail, scroll, None);
    scroll.set_flex_grow(Some(1.0));
    scroll.set_min_height(Some(Value::Point(0.0)));
    bind_always(move || rgba(theme.with(|th| th.background.raised.base)), move |c| scroll.set_background_color(Some(c)));
    scroll.set_scrollbar_options(Some(true), &ScrollBarOptions { visible: Some(Some(false)), ..ScrollBarOptions::default() });
    let content = box_();
    insert_node(scroll, content, None);
    content.set_flex_shrink(Some(0.0));
    content.set_flex_direction(FlexDirection::Column);
    content.set_gap(1, Some(Value::Point(1.0)));
    content.set_padding(Edge::Top, Some(Value::Point(if compact { 1.0 } else { 0.0 })));
    content.set_padding(Edge::Bottom, Some(Value::Point(if compact { 1.0 } else { 0.0 })));

    insert(
        content,
        for_each(
            {
                let items = items.clone();
                move || items()
            },
            |tab: &SessionTab| tab.session_id.clone(),
            {
                let controller = controller.clone();
                let statuses_memo = statuses_memo.clone();
                let items = items.clone();
                let active = active.clone();
                let hovered = hovered;
                let dragging = dragging;
                move |tab: Signal<SessionTab>, index: Signal<usize>| {
                    vertical_tab(
                        controller.clone(),
                        statuses_memo.clone(),
                        items.clone(),
                        active.clone(),
                        hovered,
                        dragging,
                        theme,
                        dimensions,
                        width,
                        compact,
                        numbers,
                        tab,
                        index,
                    )
                }
            },
            None,
        ),
        At::End,
    );

    // One slot with two states: the muted "+ New session" affordance promotes into the active new-session tab.
    let add = box_();
    insert_node(content, add, None);
    add.set_height(Some(Value::Point(1.0)));
    add.set_width(Some(Value::Percent(100.0)));
    add.set_position_type(false);
    add.set_flex_direction(FlexDirection::Row);
    add.set_padding(Edge::Left, Some(Value::Point(if compact { 0.0 } else { 1.0 })));
    add.set_justify_content(if compact { Justify::Center } else { Justify::FlexStart });
    add.set_align_items(opencode_tui_core::tree::Align::Center);
    bind_always(
        {
            let controller = controller.clone();
            let add_hovered = add_hovered;
            move || {
                let theme = theme.get();
                if controller.new_tab() && !compact {
                    rgba(theme.background.action.primary.selected)
                } else if add_hovered.get() || (compact && controller.new_tab()) {
                    rgba(theme.background.action.primary.hovered)
                } else {
                    rgba(theme.background.raised.base)
                }
            }
        },
        move |c| add.set_background_color(Some(c)),
    );
    add.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| add_hovered.set(true))));
    add.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| add_hovered.set(false))));
    {
        let controller = controller.clone();
        add.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |event| {
                if event.button == 2 || controller.new_tab() {
                    return;
                }
                controller.add();
            })),
        );
    }
    let plus = text();
    insert_node(add, plus, None);
    plus.set_width(Some(Value::Point(if compact { 1.0 } else { 2.0 })));
    plus.set_selectable(false);
    bind_always(
        {
            let controller = controller.clone();
            let add_hovered = add_hovered;
            move || {
                let theme = theme.get();
                if controller.new_tab() || add_hovered.get() {
                    rgba(theme.text.base)
                } else {
                    idle_number(&theme)
                }
            }
        },
        move |c| plus.set_fg(Some(c)),
    );
    bind(
        {
            let controller = controller.clone();
            move || controller.new_tab()
        },
        move |new| plus.set_attributes(if new { Some(text_attributes::BOLD) } else { None }),
    );
    insert(plus, View::from("+"), At::Only);
    insert(
        add,
        show(
            move || !compact,
            move || {
                let label = text();
                label.set_wrap_mode(WrapMode::None);
                label.set_selectable(false);
                bind_always(
                    {
                        let controller = controller.clone();
                        let add_hovered = add_hovered;
                        move || {
                            let theme = theme.get();
                            if controller.new_tab() || add_hovered.get() {
                                rgba(theme.text.base)
                            } else {
                                rgba(theme.text.muted)
                            }
                        }
                    },
                    move |c| label.set_fg(Some(c)),
                );
                bind(
                    {
                        let controller = controller.clone();
                        move || controller.new_tab()
                    },
                    move |new| label.set_attributes(if new { Some(text_attributes::BOLD) } else { None }),
                );
                insert(label, View::from(NEW_SESSION_TAB_TITLE), At::Only);
                View::Node(label)
            },
            None,
        ),
        At::End,
    );
    let _ = animations;
    View::Node(rail)
}

fn highlighted(
    active: &dyn Fn() -> Option<String>,
    hovered: Signal<Option<String>>,
    dragging: Signal<Option<String>>,
    session_id: &str,
) -> bool {
    active().as_deref() == Some(session_id)
        || hovered.get().as_deref() == Some(session_id)
        || dragging.get().as_deref() == Some(session_id)
}

#[allow(clippy::too_many_arguments)]
fn vertical_tab(
    controller: SessionTabs,
    statuses: Memo<Vec<TabStatus>>,
    items: Rc<dyn Fn() -> Vec<SessionTab>>,
    active: Rc<dyn Fn() -> Option<String>>,
    hovered: Signal<Option<String>>,
    dragging: Signal<Option<String>>,
    theme: Signal<ResolvedTheme>,
    dimensions: Signal<(u32, u32)>,
    width: u32,
    compact: bool,
    numbers: bool,
    tab: Signal<SessionTab>,
    index: Signal<usize>,
) -> View {
    let session_id = {
        let tab = tab;
        move || tab.get().session_id
    };
    let selected = {
        let active = active.clone();
        let session_id = session_id.clone();
        move || active().as_deref() == Some(session_id().as_str())
    };
    let status = {
        let statuses = statuses.clone();
        let index = index;
        move || statuses.get().get(index.get()).cloned().unwrap_or_default()
    };
    let number_width = {
        let items = items.clone();
        move || number_width(items().len())
    };
    let prefix_width = {
        let number_width = number_width.clone();
        move || number_width() + 1
    };
    let resting_title_width = {
        let prefix_width = prefix_width.clone();
        move || (width as i64 - prefix_width() - 1).max(1)
    };
    let title_width = {
        let resting = resting_title_width.clone();
        let hovered = hovered;
        let session_id = session_id.clone();
        move || {
            if hovered.get().as_deref() == Some(session_id().as_str()) {
                (resting() - 1).max(1)
            } else {
                resting()
            }
        }
    };

    let el = box_();
    el.set_height(Some(Value::Point(if compact { 1.0 } else { 2.0 })));
    el.set_width(Some(Value::Percent(100.0)));
    el.set_position_type(false);
    el.set_flex_direction(FlexDirection::Column);
    bind_always(
        {
            let selected = selected.clone();
            let hovered = hovered;
            let dragging = dragging;
            let session_id = session_id.clone();
            move || {
                let theme = theme.get();
                if selected() && !compact {
                    return rgba(theme.background.action.primary.selected);
                }
                if (compact && selected())
                    || hovered.get().as_deref() == Some(session_id().as_str())
                    || dragging.get().as_deref() == Some(session_id().as_str())
                {
                    return rgba(theme.background.action.primary.hovered);
                }
                rgba(theme.background.raised.base)
            }
        },
        move |c| el.set_background_color(Some(c)),
    );
    {
        let hovered = hovered;
        let session_id = session_id.clone();
        el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| hovered.set(Some(session_id())))));
    }
    {
        let hovered = hovered;
        el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| hovered.set(None))));
    }

    if compact {
        let line = box_();
        insert_node(el, line, None);
        line.set_height(Some(Value::Point(1.0)));
        line.set_flex_direction(FlexDirection::Row);
        line.set_justify_content(Justify::Center);
        let indicator = box_();
        insert_node(line, indicator, None);
        indicator.set_width(Some(Value::Point(width as f32)));
        indicator.set_flex_shrink(Some(0.0));
        indicator.set_flex_direction(FlexDirection::Row);
        indicator.set_justify_content(Justify::Center);
        let label = text();
        insert_node(indicator, label, None);
        label.set_selectable(false);
        bind(
            {
                let status = status.clone();
                let selected = selected.clone();
                let index = index;
                move || {
                    if selected() {
                        NumberOrIdle::Number(session_tab_number_label(index.get()))
                    } else {
                        let label = indicator_label(&status(), numbers, &session_tab_number_label(index.get()), false);
                        if label.is_empty() {
                            NumberOrIdle::Idle
                        } else {
                            NumberOrIdle::Number(label)
                        }
                    }
                }
            },
            move |value| {
                let tab = tab;
                let text = match value {
                    NumberOrIdle::Number(value) => value,
                    NumberOrIdle::Idle => graphemes(title_of(&tab.get()).trim_start()).first().map(|c| c.to_string()).unwrap_or_else(|| "U".into()),
                };
                label.set_content(&text);
            },
        );
        bind_always(
            {
                let status = status.clone();
                let selected = selected.clone();
                move || {
                    let theme = theme.get();
                    if selected() {
                        return rgba(theme.text.base);
                    }
                    feedback_color(&theme, &status()).unwrap_or_else(|| {
                        if status().busy && status().attention == Attention::None {
                            active_number(&theme)
                        } else {
                            rgba(theme.text.muted)
                        }
                    })
                }
            },
            move |c| label.set_fg(Some(c)),
        );
        return View::Node(el);
    }

    // First line: the number/status cell and the title.
    let first = box_();
    insert_node(el, first, None);
    first.set_height(Some(Value::Point(1.0)));
    first.set_width(Some(Value::Percent(100.0)));
    first.set_flex_direction(FlexDirection::Row);
    first.set_position_type(false);
    let first_inner = box_();
    insert_node(first, first_inner, None);
    first_inner.set_z_index(1);
    first_inner.set_width(Some(Value::Percent(100.0)));
    first_inner.set_flex_direction(FlexDirection::Row);
    first_inner.set_padding(Edge::Right, Some(Value::Point(1.0)));
    let indicator = box_();
    insert_node(first_inner, indicator, None);
    bind(
        {
            let number_width = number_width.clone();
            move || number_width() + 1
        },
        move |value| indicator.set_width(Some(Value::Point(value as f32))),
    );
    indicator.set_flex_shrink(Some(0.0));
    indicator.set_flex_direction(FlexDirection::Row);
    indicator.set_justify_content(Justify::FlexEnd);
    indicator.set_padding(Edge::Right, Some(Value::Point(1.0)));
    let indicator_text = text();
    insert_node(indicator, indicator_text, None);
    indicator_text.set_selectable(false);
    bind(
        {
            let status = status.clone();
            let index = index;
            move || indicator_label(&status(), numbers, &session_tab_number_label(index.get()), false)
        },
        move |label| indicator_text.set_content(&label),
    );
    bind_always(
        {
            let status = status.clone();
            let selected = selected.clone();
            let hovered = hovered;
            let session_id = session_id.clone();
            move || {
                let theme = theme.get();
                let status = status();
                let feedback = feedback_color(&theme, &status);
                let base = if hovered.get().as_deref() == Some(session_id().as_str()) && !selected() {
                    rgba(theme.text.base)
                } else {
                    tint(idle_number(&theme), tint(rgba(theme.text.base), rgba(theme.background.raised.base), 0.25), if selected() { 1.0 } else { 0.0 })
                };
                if status.busy && status.attention == Attention::None {
                    active_number(&theme)
                } else {
                    feedback.unwrap_or(base)
                }
            }
        },
        move |c| indicator_text.set_fg(Some(c)),
    );
    bind(
        {
            let selected = selected.clone();
            move || selected()
        },
        move |selected| indicator_text.set_attributes(if selected { Some(text_attributes::BOLD) } else { None }),
    );

    let title_el = text();
    insert_node(first_inner, title_el, None);
    title_el.set_wrap_mode(WrapMode::None);
    title_el.set_truncate(true);
    title_el.set_selectable(false);
    bind(title_width.clone(), move |value| title_el.set_width(Some(Value::Point(value as f32))));
    bind(
        {
            let title_width = title_width.clone();
            let tab = tab;
            move || take_width(&title_of(&tab.get()), title_width())
        },
        move |title| title_el.set_content(&title),
    );
    bind_always(
        {
            let selected = selected.clone();
            let hovered = hovered;
            let session_id = session_id.clone();
            move || {
                let theme = theme.get();
                if hovered.get().as_deref() == Some(session_id().as_str()) {
                    rgba(theme.text.base)
                } else if selected() {
                    rgba(theme.text.base)
                } else {
                    rgba(theme.text.muted)
                }
            }
        },
        move |c| title_el.set_fg(Some(c)),
    );
    bind(
        {
            let selected = selected.clone();
            move || selected()
        },
        move |selected| title_el.set_attributes(if selected { Some(text_attributes::BOLD) } else { None }),
    );
    let close_el = text();
    insert_node(first_inner, close_el, None);
    close_el.set_position_type(true);
    close_el.set_positions(None, None, Some(Some(Value::Point(1.0))), None);
    close_el.set_z_index(2);
    close_el.set_width(Some(Value::Point(1.0)));
    close_el.set_selectable(false);
    bind_always(
        move || {
            let theme = theme.get();
            rgba(theme.text.muted)
        },
        move |c| close_el.set_fg(Some(c)),
    );
    bind(
        {
            let hovered = hovered;
            let session_id = session_id.clone();
            move || if hovered.get().as_deref() == Some(session_id().as_str()) { "✕".to_string() } else { String::new() }
        },
        move |label| close_el.set_content(&label),
    );
    {
        let controller = controller.clone();
        let hovered = hovered;
        let session_id = session_id.clone();
        close_el.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |event| {
                if event.button == 2 || hovered.get().as_deref() != Some(session_id().as_str()) {
                    return;
                }
                event.stop_propagation();
                let id = session_id();
                controller.close(Some(&id));
            })),
        );
    }

    // Second line: the project and branch detail.
    let detail = {
        let tab = tab;
        let title_width = title_width.clone();
        move || {
            let _ = dimensions.get();
            let tab = tab.get();
            let data = tui().data.clone();
            let session = data.store.session.get(&tab.session_id);
            let detail = session.as_ref().and_then(|session| {
                let project = session.str("projectID")?;
                let project = data.store.project.get(project)?;
                let location = session.get("location").cloned();
                let directory = location.as_ref().and_then(|l| get_str(l, "directory")).unwrap_or_default();
                let canonical = match project.get("canonical") {
                    Some(opencode_protocol::Json::String(value)) => value.as_str(),
                    _ => directory,
                };
                let name = project_name(canonical, directory);
                let vcs = location.as_ref().and_then(|l| data.store.location.vcs.list(Some(l)));
                let branch = json_str(vcs.as_ref().and_then(|v| v.at("branch.current")));
                let default = json_str(vcs.as_ref().and_then(|v| v.at("branch.default")));
                let info = location.as_ref().and_then(|l| data.store.location.info(Some(l)));
                let worktree = info.as_ref().is_some_and(|info| {
                    let project = info.get("project");
                    let directory = project.and_then(|p| get_str(p, "directory"));
                    let canonical = project.and_then(|p| get_str(p, "canonical"));
                    directory.is_some() && canonical.is_some() && directory != canonical
                });
                session_tab_detail(name.as_deref().unwrap_or(""), branch.unwrap_or(""), default, worktree)
            });
            let detail = detail.unwrap_or_default();
            take_width(&detail, title_width())
        }
    };
    let _ = feedback_color(&theme.get(), &TabStatus::default());
    let second = box_();
    insert_node(el, second, None);
    second.set_height(Some(Value::Point(1.0)));
    second.set_width(Some(Value::Percent(100.0)));
    second.set_flex_direction(FlexDirection::Row);
    second.set_position_type(false);
    let second_inner = box_();
    insert_node(second, second_inner, None);
    second_inner.set_z_index(1);
    second_inner.set_width(Some(Value::Percent(100.0)));
    second_inner.set_flex_direction(FlexDirection::Row);
    second_inner.set_padding(Edge::Left, Some(Value::Point(prefix_width() as f32)));
    second_inner.set_padding(Edge::Right, Some(Value::Point(2.0)));
    let detail_el = text();
    insert_node(second_inner, detail_el, None);
    detail_el.set_wrap_mode(WrapMode::None);
    detail_el.set_selectable(false);
    bind_always(
        {
            let theme = theme;
            move || {
                let theme = theme.get();
                tint(rgba(theme.text.muted), rgba(theme.background.raised.base), 0.35)
            }
        },
        move |c| detail_el.set_fg(Some(c)),
    );
    bind(detail, move |value| detail_el.set_content(&value));

    View::Node(el)
}

#[derive(Clone, PartialEq)]
enum NumberOrIdle {
    Number(String),
    Idle,
}

/// A JSON string value.
fn json_str(value: Option<&opencode_protocol::Json>) -> Option<&str> {
    match value {
        Some(opencode_protocol::Json::String(text)) => Some(text),
        _ => None,
    }
}

/// `projectName(project, fallback)` (`util/project.ts`).
fn project_name(canonical: &str, fallback: &str) -> Option<String> {
    let canonical = if canonical.is_empty() { fallback } else { canonical };
    if canonical == "/" {
        return if fallback.is_empty() {
            None
        } else {
            std::path::Path::new(fallback).file_name().map(|n| n.to_string_lossy().into_owned())
        };
    }
    Some(std::path::Path::new(canonical).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| canonical.to_string()))
}

/// `SessionTabsRailControls`: the compact rail's number/plus control rows.
fn rail_controls(
    _controller: &SessionTabs,
    width: u32,
    below_highlighted: Rc<dyn Fn() -> bool>,
    theme: Signal<ResolvedTheme>,
) -> View {
    let el = box_();
    el.set_flex_direction(FlexDirection::Column);
    el.set_width(Some(Value::Point(width as f32)));
    el.set_flex_shrink(Some(0.0));
    el.set_background_color(Some(rgba(theme.with(|t| t.background.raised.base))));
    let _ = below_highlighted();
    View::Node(el)
}
