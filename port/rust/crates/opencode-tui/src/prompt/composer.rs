//! The session composer (`routes/session/composer/index.tsx`, `context.ts`, `shell-tab.tsx`): a panel over the
//! prompt with tabs (subagents, shells, terminals), switched with left and right, closed with escape or ctrl+c.
//!
//! Mount points: the subagents tab is the tools slice's ([`crate::subagents::subagents_tab`]), the terminals tab
//! the tabs slice's; each tab gets the composer's context (`useComposerTab()`) as a [`ComposerTabApi`] prop and
//! registers itself (`register`), shows while `active(id)`, and may `close()` the composer.
//!
//! TODO(port-tui): the terminals tab (the tabs slice); the shell output dialog (`DialogShellOutput`, the dialogs
//! slice).
use std::cell::Cell;
use std::rc::Rc;

use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{b, box_, scrollbox, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::scrollbox::{ScrollBarOptions, ScrollUnit};
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, Justify, MouseEventType};
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::tui;
use crate::keymap::{self, Command, Enabled, Layer, create_layer, use_shortcuts};
pub use crate::subagents::{ComposerHint, ComposerTab, ComposerTabApi};
use crate::util::rgba;

/// `ComposerProps`.
pub struct ComposerProps {
    pub session_id: String,
    pub open: Rc<dyn Fn() -> bool>,
    pub default_tab: Rc<dyn Fn() -> Option<String>>,
    pub on_close: Option<Rc<dyn Fn()>>,
    pub terminals: Rc<dyn Fn() -> bool>,
    pub visible_terminal_id: Rc<dyn Fn() -> Option<String>>,
}

#[derive(Clone)]
struct Tab(ComposerTab);

impl PartialEq for Tab {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id && self.0.label == other.0.label
    }
}

/// `<Composer>`.
pub fn composer(props: ComposerProps) -> View {
    let theme = tui().theme;
    // `store.tabs` (by id, in registration order) and `store.active`.
    let tabs = create_signal(Vec::<Tab>::new()).0;
    let active = create_signal(String::new()).0;
    let tab_list = create_memo(move || tabs.get());
    let active_tab = create_memo(move || {
        let current = active.get();
        tab_list.with(|list| list.iter().find(|t| t.0.id == current).cloned())
    });
    let footer_hints = create_memo(move || active_tab.with(|t| t.as_ref().and_then(|t| t.0.hints.as_ref().map(|h| h()))).unwrap_or_default());

    // The active tab when opened.
    {
        let open = props.open.clone();
        let default_tab = props.default_tab.clone();
        create_effect(move || {
            if !open() {
                return;
            }
            let list = tab_list.get();
            let Some(first) = list.first() else { return };
            let wanted = default_tab();
            let id = wanted.and_then(|w| list.iter().find(|t| t.0.id == w).map(|t| t.0.id)).unwrap_or(first.0.id);
            active.set(id.to_string());
        });
    }
    let close: Rc<dyn Fn()> = {
        let on_close = props.on_close.clone();
        Rc::new(move || {
            if let Some(on_close) = &on_close {
                on_close();
            }
        })
    };
    let api = {
        let open = props.open.clone();
        ComposerTabApi {
            register: Rc::new(move |tab: ComposerTab| {
                let id = tab.id;
                tabs.update(|list| match list.iter_mut().find(|t| t.0.id == id) {
                    Some(existing) => existing.0 = tab,
                    None => list.push(Tab(tab)),
                });
                if active.get_untracked().is_empty() {
                    active.set(id.to_string());
                }
                Box::new(move || {
                    if tabs.is_alive() {
                        tabs.update(|list| list.retain(|t| t.0.id != id));
                    }
                })
            }),
            active: Rc::new(move |id: &str| open() && active.with(|a| a == id)),
            close: close.clone(),
        }
    };
    {
        let open = props.open.clone();
        create_effect(move || {
            if !open() {
                return;
            }
            let pop = keymap::mode_push("composer", None);
            on_cleanup(pop);
        });
    }
    let switch_tab = move |direction: i64| {
        let list = tab_list.get_untracked();
        if list.len() <= 1 {
            return;
        }
        let current = active.get_untracked();
        let index = list.iter().position(|t| t.0.id == current).map(|i| i as i64).unwrap_or(-1);
        let len = list.len() as i64;
        active.set(list[((index + direction + len) % len) as usize].0.id.to_string());
    };
    {
        let open = props.open.clone();
        let close = close.clone();
        create_layer(move || {
            let open = open.clone();
            let close_escape = close.clone();
            let close_ctrl = close.clone();
            Layer {
                mode: Some("composer"),
                enabled: Some(Enabled::Fn(Rc::new(move || open()))),
                priority: Some(1),
                commands: vec![
                    Command::inline("left", move || {
                        switch_tab(-1);
                        true
                    })
                    .title("Previous tab")
                    .group("Composer"),
                    Command::inline("right", move || {
                        switch_tab(1);
                        true
                    })
                    .title("Next tab")
                    .group("Composer"),
                    Command::inline("escape", move || {
                        close_escape();
                        true
                    })
                    .title("Close composer")
                    .group("Composer"),
                    Command::inline("ctrl+c", move || {
                        close_ctrl();
                        true
                    })
                    .title("Close composer")
                    .group("Composer"),
                ],
                ..Layer::default()
            }
        });
    }

    let root = box_();
    root.set_flex_shrink(Some(0.0));
    {
        let open = props.open.clone();
        bind(move || open(), move |v| root.set_visible(v));
    }
    let panel = box_();
    insert_node(root, panel, None);
    panel.set_border(BorderProp::Sides(vec![Side::Left]));
    panel.set_custom_border_chars(Some(opencode_tui_core::border::border_chars_to_array(&opencode_tui_core::border::BorderCharacters {
        top_left: "",
        top_right: "",
        bottom_left: "",
        bottom_right: "",
        horizontal: " ",
        vertical: "┃",
        top_t: "",
        bottom_t: "",
        left_t: "",
        right_t: "",
        cross: "",
    })));
    bind_always(move || rgba(theme.with(|th| th.border.base)), move |c| panel.set_border_color(Some(c)));
    bind_always(move || rgba(theme.with(|th| th.background.raised.base)), move |c| panel.set_background_color(Some(c)));
    panel.set_padding(Edge::Left, Some(Value::Point(1.0)));
    panel.set_padding(Edge::Right, Some(Value::Point(2.0)));
    panel.set_padding(Edge::Top, Some(Value::Point(1.0)));
    panel.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    let body = box_();
    insert_node(panel, body, None);
    body.set_gap(2, Some(Value::Point(1.0)));

    // The header: the tabs (or the only tab's label) and "esc".
    let header = box_();
    insert_node(body, header, None);
    header.set_flex_direction(FlexDirection::Row);
    header.set_justify_content(Justify::SpaceBetween);
    header.set_padding(Edge::Left, Some(Value::Point(1.0)));
    let labels = show(
        move || tab_list.with(|l| l.len() > 1),
        move || {
            let row = box_();
            row.set_flex_direction(FlexDirection::Row);
            row.set_gap(2, Some(Value::Point(2.0)));
            insert(
                row,
                for_each(
                    move || tab_list.get(),
                    |tab| tab.0.id,
                    move |tab, _| {
                        let is_active = create_memo(move || active.get() == tab.with(|t| t.0.id));
                        let el = text();
                        bind_always(
                            move || if is_active.get() { rgba(theme.with(|th| th.text.base)) } else { rgba(theme.with(|th| th.text.muted)) },
                            move |c| el.set_fg(Some(c)),
                        );
                        bind(move || is_active.get().then_some(text_attributes::BOLD), move |a| el.set_attributes(a));
                        insert(el, View::dynamic(move || View::from(tab.with(|t| t.0.label))), At::Only);
                        View::Node(el)
                    },
                    None,
                ),
                At::Only,
            );
            View::Node(row)
        },
        Some(Rc::new(move || {
            let el = text();
            bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| el.set_fg(Some(c)));
            el.set_attributes(Some(text_attributes::BOLD));
            insert(el, View::dynamic(move || View::from(tab_list.with(|l| l.first().map(|t| t.0.label).unwrap_or("")))), At::Only);
            View::Node(el)
        })),
    );
    insert(header, labels, At::End);
    let esc = text();
    insert_node(header, esc, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| esc.set_fg(Some(c)));
    {
        let close = close.clone();
        esc.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| close())));
    }
    insert(esc, View::from("esc"), At::Only);

    // The tabs.
    {
        let api = api.clone();
        let session_id = props.session_id.clone();
        insert(
            body,
            component(move || {
                crate::subagents::subagents_tab(crate::subagents::SubagentsTabProps { session_id: session_id.clone(), composer: api.clone() })
            }),
            At::End,
        );
    }
    {
        let api = api.clone();
        let session_id = props.session_id.clone();
        insert(body, component(move || shell_tab(session_id.clone(), api.clone())), At::End);
    }
    // TODO(port-tui): the tabs slice: `<TerminalsTab sessionID visibleTerminalID>` while `props.terminals`.
    let _ = (props.terminals.clone(), props.visible_terminal_id.clone());

    // The footer: the active tab's hints, and the tab keys.
    let footer = box_();
    insert_node(body, footer, None);
    footer.set_flex_direction(FlexDirection::Row);
    footer.set_gap(2, Some(Value::Point(2.0)));
    footer.set_padding(Edge::Left, Some(Value::Point(1.0)));
    footer.set_flex_shrink(Some(0.0));
    let hint = move |label: View, shortcut: View| {
        let el = text();
        let strong = span();
        insert_node(el, strong, None);
        bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| strong.set_span_style(Some(c), None, 0, 0));
        let bold = b();
        insert_node(strong, bold, None);
        insert(bold, label, At::Only);
        insert(strong, View::from(" "), At::End);
        let muted = span();
        insert_node(el, muted, None);
        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| muted.set_span_style(Some(c), None, 0, 0));
        insert(muted, shortcut, At::Only);
        View::Node(el)
    };
    insert(
        footer,
        for_each(
            move || footer_hints.get(),
            |h| (h.label.clone(), h.shortcut.clone()),
            move |h, _| {
                hint(
                    View::dynamic(move || View::from(h.with(|h| h.label.clone()))),
                    View::dynamic(move || View::from(h.with(|h| h.shortcut.clone()))),
                )
            },
            None,
        ),
        At::End,
    );
    insert(footer, show(move || tab_list.with(|l| l.len() > 1), move || hint(View::from("tabs"), View::from("←/→")), None), At::End);
    View::Node(root)
}

/// `<ShellTab sessionID>`: the session's running shell commands.
fn shell_tab(session_id: String, composer: ComposerTabApi) -> View {
    let t = tui();
    let theme = t.theme;
    let store = t.data.store.clone();
    let client = t.data.client.clone();
    let shortcuts = use_shortcuts();
    let entries = {
        let store = store.clone();
        let session_id = session_id.clone();
        create_memo(move || {
            store
                .shell
                .list_by_session(&session_id)
                .into_iter()
                .filter(|shell| shell.str("status") == Some("running"))
                .collect::<Vec<_>>()
        })
    };
    let selected = create_signal(0usize).0;
    let scroll: Rc<Cell<Option<Element>>> = Rc::new(Cell::new(None));
    let selected_entry = create_memo(move || {
        let index = selected.get();
        entries.with(|e| e.get(index).cloned())
    });
    // TODO(port-tui): the dialogs slice: `DialogShellOutput` for the selected entry.
    let open = Rc::new(move || {
        let _ = selected_entry.get_untracked();
    });
    create_effect(move || {
        let count = entries.with(|e| e.len());
        if selected.get() >= count {
            selected.set(count.saturating_sub(1));
        }
    });
    {
        let scroll = scroll.clone();
        create_effect(move || {
            let index = selected.get();
            let Some(scroll) = scroll.get().filter(|s| s.is_alive()) else { return };
            let Some(target) = scroll.children().get(index).copied() else { return };
            let y = target.y() - scroll.y();
            let height = scroll.height() as i32;
            if y >= height || y < 0 {
                scroll.scroll_by((y - height / 2) as f64, ScrollUnit::Absolute);
            }
        });
    }
    {
        let composer = composer.clone();
        on_mount(move || {
            let cleanup = (composer.register)(ComposerTab {
                id: "shell",
                label: "Shell",
                hints: Some(Rc::new(move || {
                    if selected_entry.get().is_none() {
                        return vec![];
                    }
                    vec![
                        ComposerHint { label: "output".into(), shortcut: shortcuts.get("composer.shell.select").unwrap_or_default() },
                        ComposerHint { label: "kill".into(), shortcut: shortcuts.get("composer.shell.kill").unwrap_or_default() },
                    ]
                })),
            });
            on_cleanup(cleanup);
        });
    }
    {
        let composer = composer.clone();
        let open = open.clone();
        create_layer(move || {
            let active = composer.active.clone();
            let close = composer.close.clone();
            let open = open.clone();
            let client = client.clone();
            Layer {
                mode: Some("composer"),
                enabled: Some(Enabled::Fn(Rc::new(move || active("shell")))),
                priority: Some(1),
                commands: vec![
                    Command::new("composer.shell.up", move || {
                        if selected.get_untracked() == 0 {
                            close();
                            return;
                        }
                        selected.set(selected.get_untracked() - 1);
                    })
                    .title("Previous shell")
                    .group("Composer"),
                    Command::new("composer.shell.down", move || {
                        let count = entries.with_untracked(|e| e.len());
                        if count == 0 {
                            return;
                        }
                        selected.set((selected.get_untracked() + 1) % count);
                    })
                    .title("Next shell")
                    .group("Composer"),
                    Command::new("composer.shell.select", move || open()).title("View shell output").group("Composer"),
                    Command::new("composer.shell.kill", move || {
                        let Some(entry) = selected_entry.get_untracked() else { return };
                        let input = obj! {
                            "id" => entry.str("id").unwrap_or_default(),
                            "location" => obj! { "directory" => entry.at("location.directory").cloned() },
                        };
                        let call = client.call_named("shell.remove", &input, RequestOptions::default());
                        tokio::task::spawn_local(async move {
                            let _ = call.await;
                        });
                    })
                    .title("Kill shell command")
                    .group("Composer"),
                ],
                ..Layer::default()
            }
        });
    }
    let active = composer.active.clone();
    show(
        move || active("shell"),
        move || {
            let el = scrollbox();
            scroll.set(Some(el));
            el.set_scrollbar_options(Some(true), &ScrollBarOptions { visible: Some(Some(false)), ..ScrollBarOptions::default() });
            el.set_max_height(Some(Value::Point(5.0)));
            let open = open.clone();
            insert(
                el,
                show(
                    move || entries.with(|e| !e.is_empty()),
                    move || {
                        let open = open.clone();
                        for_each(
                            move || entries.get(),
                            |shell| shell.str("id").unwrap_or_default().to_string(),
                            move |shell, index| {
                                let is_active = create_memo(move || index.get() == selected.get());
                                let row = box_();
                                row.set_flex_direction(FlexDirection::Row);
                                row.set_padding(Edge::Left, Some(Value::Point(1.0)));
                                row.set_padding(Edge::Right, Some(Value::Point(1.0)));
                                bind_always(
                                    move || {
                                        if is_active.get() {
                                            rgba(theme.with(|th| th.background.action.primary.focused))
                                        } else {
                                            rgba(theme.with(|th| th.background.action.primary.base))
                                        }
                                    },
                                    move |c| row.set_background_color(Some(c)),
                                );
                                row.set_on_mouse(Some(MouseEventType::Move), Some(Rc::new(move |_| selected.set(index.get_untracked()))));
                                {
                                    let open = open.clone();
                                    row.set_on_mouse(
                                        Some(MouseEventType::Up),
                                        Some(Rc::new(move |_| {
                                            selected.set(index.get_untracked());
                                            open();
                                        })),
                                    );
                                }
                                let label = text();
                                insert_node(row, label, None);
                                bind_always(
                                    move || {
                                        if is_active.get() {
                                            rgba(theme.with(|th| th.text.action.primary.focused))
                                        } else {
                                            rgba(theme.with(|th| th.text.action.primary.base))
                                        }
                                    },
                                    move |c| label.set_fg(Some(c)),
                                );
                                bind(move || is_active.get().then_some(text_attributes::BOLD), move |a| label.set_attributes(a));
                                label.set_wrap_mode(WrapMode::None);
                                insert(
                                    label,
                                    View::dynamic(move || {
                                        View::from(shell.with(|s| s.str("command").unwrap_or_default().split('\n').next().unwrap_or("").to_string()))
                                    }),
                                    At::Only,
                                );
                                View::Node(row)
                            },
                            None,
                        )
                    },
                    Some(Rc::new(move || {
                        let el = text();
                        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| el.set_fg(Some(c)));
                        insert(el, View::from(" No shell commands"), At::Only);
                        View::Node(el)
                    })),
                ),
                At::Only,
            );
            View::Node(el)
        },
        None,
    )
}

