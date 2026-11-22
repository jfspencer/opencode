//! `component/session-frame.tsx` (`SessionFrame`) and `routes/session/sidebar.tsx` (`Sidebar`): the session pane
//! with its right-hand sidebar, the narrow-terminal sidebar overlay, and the commands that toggle it.
//!
//! TODO(port-tui): the terminal pane, panels and the pane resize handle (`session-terminals`, `context/panel`,
//! `ui/pane-resize*`); the sidebar's built-in slots (`sidebar.content`/`sidebar.footer`) carry the context usage and
//! location footer here, without the plugin system.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, scrollbox, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::scrollbox::ScrollBarOptions;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Align, Edge, FlexDirection, MouseEventType};
use opencode_tui_sync::json::JsonExt;

use crate::ctx::tui;
use crate::keymap::create_layer;
use crate::session::route::{SessionProps, session};
use crate::util::{abbreviate_home, rgba, truncate_file_path};

/// `SESSION_SIDEBAR_WIDTH` (`ui/layout.ts`).
const SESSION_SIDEBAR_WIDTH: u32 = 42;

/// What the right pane shows. Terminals and panels are not ported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RightPane {
    None,
    Sidebar,
}

/// `<SessionFrame sessionID verticalTabsWidth>`.
pub fn session_frame(session_id: String, vertical_tabs_width: Rc<dyn Fn() -> u32>) -> View {
    let t = tui();
    let dimensions = t.dimensions;
    let theme = t.theme;
    let config = t.config_context.clone();
    let dialog = crate::ui::dialog::use_dialog();
    let sidebar_open = create_signal(false).0;
    let session_width: Signal<Option<f32>> = create_signal(None::<f32>).0;
    let session_node = box_();

    let wide: Rc<dyn Fn() -> bool> = {
        let vertical_tabs_width = vertical_tabs_width.clone();
        Rc::new(move || dimensions.get().0 as i64 - vertical_tabs_width() as i64 > 120)
    };
    let sidebar_visible: Rc<dyn Fn() -> bool> = {
        let data = t.data.clone();
        let session_id = session_id.clone();
        let config = config.clone();
        let wide = wide.clone();
        Rc::new(move || {
            if data.store.session.get(&session_id).is_some_and(|session| session.get("parentID").is_some_and(|p| !matches!(p, Json::Null))) {
                return false;
            }
            if sidebar_open.get() {
                return true;
            }
            config.data().session.sidebar.as_deref().unwrap_or("auto") == "auto" && wide()
        })
    };
    let right_pane = {
        let sidebar_visible = sidebar_visible.clone();
        move || {
            if sidebar_open.get() && sidebar_visible() {
                return RightPane::Sidebar;
            }
            if sidebar_visible() {
                return RightPane::Sidebar;
            }
            RightPane::None
        }
    };
    let toggle_sidebar = {
        let config = config.clone();
        let toast = t.toast.clone();
        let sidebar_open = sidebar_open;
        let right_pane = right_pane.clone();
        Rc::new(move || {
            batch(|| {
                let visible = right_pane() == RightPane::Sidebar;
                if let Err(error) = config.update(|draft| {
                    let session = draft.get("session").cloned().filter(|value| matches!(value, Json::Object(_)));
                    let mut session = match session {
                        Some(Json::Object(session)) => session,
                        _ => Default::default(),
                    };
                    session.insert("sidebar".into(), Json::String(if visible { "hide" } else { "auto" }.into()));
                    if let Json::Object(object) = draft {
                        object.insert("session".into(), Json::Object(session));
                    }
                }) {
                    toast.error(error);
                }
                sidebar_open.set(!visible);
            });
        })
    };
    {
        let toggle = toggle_sidebar.clone();
        let dialog = dialog.clone();
        let right_pane = right_pane.clone();
        create_layer(move || {
            let toggle = toggle.clone();
            let dialog = dialog.clone();
            let right_pane = right_pane.clone();
            crate::keymap::Layer {
                mode: Some("global"),
                commands: vec![crate::keymap::Command::new("session.sidebar.toggle", move || {
                    toggle();
                    dialog.clear();
                })
                .title(if right_pane() == RightPane::Sidebar { "Hide sidebar" } else { "Show sidebar" })
                .group("Session")
                .palette(true)],
                ..crate::keymap::Layer::default()
            }
        });
    }

    let outer = box_();
    outer.set_flex_grow(Some(1.0));
    outer.set_min_width(Some(Value::Point(0.0)));
    outer.set_min_height(Some(Value::Point(0.0)));
    outer.set_flex_direction(FlexDirection::Row);
    outer.set_position_type(false);

    insert_node(outer, session_node, None);
    session_node.set_flex_grow(Some(1.0));
    session_node.set_flex_basis(Value::Point(0.0));
    session_node.set_min_width(Some(Value::Point(0.0)));
    session_node.set_min_height(Some(Value::Point(0.0)));
    session_node.set_position_type(false);
    session_node.set_height(Some(Value::Percent(100.0)));
    {
        let session_width = session_width;
        let node = session_node;
        session_node.set_on_size_change(Some(Rc::new(move || session_width.set(Some(node.width())))));
    }
    {
        let vertical_tabs_width = vertical_tabs_width.clone();
        let sidebar_visible = sidebar_visible.clone();
        let on_toggle = {
            let toggle = toggle_sidebar.clone();
            Rc::new(move || toggle())
        };
        insert(
            session_node,
            component(move || {
                let sidebar_visible = sidebar_visible.clone();
                let on_toggle = on_toggle.clone();
                let vertical_tabs_width = vertical_tabs_width.clone();
                session(SessionProps {
                    scroll_ref: None,
                    vertical_tabs_width,
                    prompt_muted: Rc::new(|| false),
                    sidebar_visible: Rc::new(move || sidebar_visible()),
                    on_toggle_sidebar: on_toggle,
                    terminals: Rc::new(|| false),
                    visible_terminal_id: Rc::new(|| None),
                    on_terminal_picker: None,
                    width: Rc::new(move || session_width.get()),
                })
            }),
            At::Only,
        );
    }

    // The docked sidebar.
    insert(
        outer,
        show(
            {
                let right_pane = right_pane.clone();
                move || right_pane() == RightPane::Sidebar
            },
            {
                let session_id = session_id.clone();
                move || {
                    let pane = box_();
                    pane.set_flex_shrink(Some(0.0));
                    pane.set_width(Some(Value::Point(SESSION_SIDEBAR_WIDTH as f32)));
                    pane.set_min_width(Some(Value::Point(0.0)));
                    pane.set_min_height(Some(Value::Point(0.0)));
                    let session_id = session_id.clone();
                    insert(pane, component(move || sidebar(session_id.clone())), At::Only);
                    View::Node(pane)
                }
            },
            None,
        ),
        At::End,
    );

    // The overlay sidebar on terminals too narrow to dock it.
    insert(
        outer,
        show(
            {
                let right_pane = right_pane.clone();
                move || right_pane() == RightPane::Sidebar && !wide()
            },
            {
                let session_id = session_id.clone();
                move || {
                    let overlay = box_();
                    overlay.set_position_type(true);
                    overlay.set_positions(
                        Some(Some(Value::Point(0.0))),
                        Some(Some(Value::Point(0.0))),
                        Some(Some(Value::Point(0.0))),
                        Some(Some(Value::Point(0.0))),
                    );
                    overlay.set_align_items(Align::FlexEnd);
                    overlay.set_background_color(Some(RGBA::from_values(0.0, 0.0, 0.0, 70.0 / 255.0)));
                    let session_id = session_id.clone();
                    insert(overlay, component(move || sidebar(session_id.clone())), At::Only);
                    View::Node(overlay)
                }
            },
            None,
        ),
        At::End,
    );
    let _ = theme;
    let _ = MouseEventType::Down;
    View::Node(outer)
}

/// `<Sidebar sessionID>`.
fn sidebar(session_id: String) -> View {
    let t = tui();
    let theme = t.theme;
    let data = t.data.clone();
    let config = t.config_context.clone();
    let session = {
        let data = data.clone();
        let session_id = session_id.clone();
        create_memo_equals(move || data.store.session.get(&session_id), Some(crate::session::parts::same_record))
    };
    let root = box_();
    root.set_width(Some(Value::Point(SESSION_SIDEBAR_WIDTH as f32)));
    root.set_height(Some(Value::Percent(100.0)));
    root.set_padding(Edge::Top, Some(Value::Point(1.0)));
    root.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    root.set_padding(Edge::Left, Some(Value::Point(2.0)));
    root.set_padding(Edge::Right, Some(Value::Point(2.0)));
    root.set_position_type(false);
    bind_always(move || rgba(theme.with(|th| th.background.raised.base)), move |c| root.set_background_color(Some(c)));

    // The title.
    let title_box = box_();
    insert_node(root, title_box, None);
    title_box.set_flex_shrink(Some(0.0));
    title_box.set_padding(Edge::Right, Some(Value::Point(2.0)));
    title_box.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    let title = text();
    insert_node(title_box, title, None);
    title.set_wrap_mode(WrapMode::None);
    title.set_truncate(true);
    title.set_attributes(Some(opencode_tui_core::color::text_attributes::BOLD));
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| title.set_fg(Some(c)));
    bind(
        {
            let session = session.clone();
            move || session.get().as_deref().map(crate::subagents::with_timestamped_fallback).unwrap_or_default()
        },
        move |value| title.set_content(&value),
    );

    // The content: the built-in context usage panel.
    let scroll = scrollbox();
    insert_node(root, scroll, None);
    scroll.set_flex_grow(Some(1.0));
    scroll.set_min_height(Some(Value::Point(0.0)));
    scroll.set_scrollbar_options(Some(true), &ScrollBarOptions { visible: Some(Some(false)), ..ScrollBarOptions::default() });
    let content = box_();
    insert_node(scroll, content, None);
    content.set_flex_shrink(Some(0.0));
    content.set_flex_direction(FlexDirection::Column);
    content.set_gap(1, Some(Value::Point(1.0)));
    content.set_padding(Edge::Right, Some(Value::Point(1.0)));
    insert(
        content,
        component({
            let session_id = session_id.clone();
            move || sidebar_context(session_id.clone())
        }),
        At::Only,
    );

    // The footer: the location and branch.
    let footer = box_();
    insert_node(root, footer, None);
    footer.set_flex_shrink(Some(0.0));
    footer.set_flex_direction(FlexDirection::Column);
    footer.set_gap(1, Some(Value::Point(1.0)));
    footer.set_padding(Edge::Top, Some(Value::Point(1.0)));
    let path = text();
    insert_node(footer, path, None);
    path.set_wrap_mode(WrapMode::None);
    path.set_truncate(true);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| path.set_fg(Some(c)));
    bind(
        {
            let data = data.clone();
            let session_id = session_id.clone();
            let config = config.clone();
            move || {
                let _ = config.data();
                let session = data.store.session.get(&session_id)?;
                let location = session.get("location")?.clone();
                let directory = location.str("directory").unwrap_or_default();
                let label = abbreviate_home(directory, &tui().paths.home);
                let branch = data.store.location.vcs.list(Some(&location)).and_then(|vcs| match vcs.at("branch.current") {
                    Some(Json::String(branch)) => Some(branch.clone()),
                    _ => None,
                });
                Some(truncate_file_path(&match branch {
                    Some(branch) => format!("{label}:{branch}"),
                    None => label,
                }, 38))
            }
        },
        move |value| path.set_content(value.as_deref().unwrap_or("")),
    );
    let _ = session;
    View::Node(root)
}

/// `SidebarContext` (`feature-plugins/sidebar/context.tsx`).
fn sidebar_context(session_id: String) -> View {
    let t = tui();
    let theme = t.theme;
    let data = t.data.clone();
    let state = {
        let data = data.clone();
        let session_id = session_id.clone();
        create_memo_equals(
            move || {
                let messages = data.store.session.message.list(&session_id);
                let session = data.store.session.get(&session_id);
                let location = session.as_ref().and_then(|s| s.get("location")).cloned();
                let models = data.store.location.model.list(location.as_ref());
                let boundary = session.as_ref().and_then(|s| s.get("revert")).and_then(|r| r.str("messageID")).map(str::to_string);
                usage(&messages, models.as_deref(), boundary.as_deref())
            },
            None,
        )
    };
    let cost = {
        let data = data.clone();
        let session_id = session_id.clone();
        create_memo(move || data.store.session.cost(&session_id))
    };
    let root = box_();
    root.set_flex_direction(FlexDirection::Column);
    insert(
        root,
        show(
            move || state.get().is_some() || cost.get() > 0.0,
            {
                let state = state.clone();
                move || {
                    let el = box_();
                    el.set_flex_direction(FlexDirection::Column);
                    let heading = text();
                    insert_node(el, heading, None);
                    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| heading.set_fg(Some(c)));
                    heading.set_attributes(Some(opencode_tui_core::color::text_attributes::BOLD));
                    insert(heading, View::from("Context"), At::Only);
                    insert(
                        el,
                        show(
                            {
                                let state = state.clone();
                                move || state.get().is_some()
                            },
                            {
                                let state = state.clone();
                                move || {
                                    let tokens = text();
                                    insert_node(el, tokens, None);
                                    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| tokens.set_fg(Some(c)));
                                    let state_tokens = state.clone();
                                    bind(
                                        move || state_tokens.get().map(|(tokens, _)| format!("{} tokens", grouped(tokens as i64))).unwrap_or_default(),
                                        move |value| tokens.set_content(&value),
                                    );
                                    let percent = text();
                                    insert_node(el, percent, None);
                                    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| percent.set_fg(Some(c)));
                                    bind(
                                        {
                                            let state = state.clone();
                                            move || state.get().and_then(|(_, percent)| percent).map(|percent| format!("{percent}% used")).unwrap_or_default()
                                        },
                                        move |value| percent.set_content(&value),
                                    );
                                    View::list([View::Node(tokens), View::Node(percent)])
                                }
                            },
                            None,
                        ),
                        At::End,
                    );
                    let spent = text();
                    insert_node(el, spent, None);
                    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| spent.set_fg(Some(c)));
                    bind(move || if cost.get() > 0.0 { format!("{:.2} spent", cost.get()) } else { String::new() }, move |value| spent.set_content(&value));
                    insert(el, View::Node(spent), At::End);
                    View::Node(el)
                }
            },
            None,
        ),
        At::Only,
    );
    View::Node(root)
}

/// `contextUsage(messages, models, boundary)`.
fn usage(
    messages: &[Rc<Json>],
    models: Option<&Json>,
    boundary: Option<&str>,
) -> Option<(f64, Option<i64>)> {
    let index = opencode_tui_sync::usage::last_assistant_with_usage(messages, boundary)?;
    let last = &messages[index];
    let tokens = last.get("tokens")?;
    let value = |path: &str| tokens.at(path).and_then(|value| match value {
        Json::Number(number) => Some(*number),
        _ => None,
    }).unwrap_or(0.0);
    let total = value("input") + value("output") + value("reasoning") + value("cache.read") + value("cache.write");
    if total <= 0.0 {
        return None;
    }
    let provider = last.at("model.providerID").and_then(|value| match value {
        Json::String(provider) => Some(provider.as_str()),
        _ => None,
    });
    let id = last.at("model.id").and_then(|value| match value {
        Json::String(id) => Some(id.as_str()),
        _ => None,
    });
    let context = models.map(crate::data::array).unwrap_or(&[]).iter().find_map(|model| {
        if model.str("providerID") == provider && model.str("id") == id {
            model.at("limit.context").and_then(|value| match value {
                Json::Number(number) if *number != 0.0 => Some(*number),
                _ => None,
            })
        } else {
            None
        }
    });
    Some((total, context.map(|context| ((total / context) * 100.0).round() as i64)))
}

/// `Intl.NumberFormat("en-US")`'s `toLocaleString()` for non-negative integers.
fn grouped(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if value < 0 {
        out.insert(0, '-');
    }
    out
}
