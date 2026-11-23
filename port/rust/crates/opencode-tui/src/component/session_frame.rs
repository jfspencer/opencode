//! `component/session-frame.tsx` (`SessionFrame`) and `routes/session/sidebar.tsx` (`Sidebar`): the session pane
//! with its right-hand pane (the terminal, the diff viewer, or the sidebar), the narrow-terminal sidebar overlay,
//! the persistent terminals (`context/session-terminals.tsx`) and the commands that toggle it.
//!
//! TODO(port-tui): the pane resize handle (`ui/pane-resize*`), the terminal picker's storage of the last selected
//! terminal across launches, and the sidebar's limit and MCP slots; the built-in slots
//! (`sidebar.content`/`sidebar.footer`) carry the context usage, todo list and location footer here, without the
//! plugin system. Panels (`context/panel.tsx`) are the diff viewer's docked and full-screen hosts.
use std::cell::RefCell;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, scrollbox, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::scrollbox::ScrollBarOptions;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Align, Edge, FlexDirection};
use opencode_tui_sync::client::{Client, Error, RequestOptions};
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::component::terminal_pane::{TerminalPaneProps, terminal_pane};
use crate::ctx::tui;
use crate::keymap::{Bind, Command, create_layer};
use crate::session::route::{SessionProps, session};
use crate::util::{abbreviate_home, rgba, truncate_file_path};

/// `SESSION_SIDEBAR_WIDTH` (`ui/layout.ts`).
const SESSION_SIDEBAR_WIDTH: u32 = 42;
/// `SESSION_CONTENT_MIN_WIDTH` (`ui/layout.ts`).
const SESSION_CONTENT_MIN_WIDTH: i64 = 44;

/// A callback another part of the frame holds (`onFocusRequest`, `onTerminalPicker`).
type FocusCallback = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// What the right pane shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RightPane {
    None,
    Sidebar,
    Panel,
    Terminal,
}

/// `<SessionFrame sessionID verticalTabsWidth>`.
pub fn session_frame(session_id: String, vertical_tabs_width: Rc<dyn Fn() -> u32>) -> View {
    let t = tui();
    let dimensions = t.dimensions;
    let config = t.config_context.clone();
    let dialog = crate::ui::dialog::use_dialog();
    let sidebar_open = create_signal(false).0;
    let session_width: Signal<Option<f32>> = create_signal(None::<f32>).0;
    let session_node = box_();
    let panel_target = {
        let session_id = session_id.clone();
        create_memo(move || t.panel.get().filter(|panel| panel.name == "diff" && panel.session_id == session_id))
    };
    let available_width: Rc<dyn Fn() -> i64> = {
        let vertical_tabs_width = vertical_tabs_width.clone();
        Rc::new(move || (dimensions.get().0 as i64 - vertical_tabs_width() as i64).max(0))
    };
    // `PanelProvider.presentation`: the requested presentation, forced full screen when the terminal cannot split.
    let panel_fullscreen: Rc<dyn Fn() -> bool> = {
        let panel_target = panel_target.clone();
        let available_width = available_width.clone();
        Rc::new(move || panel_target.get().is_some() && (t.panel_fullscreen.get() || available_width() < 64 + 44))
    };
    let panel_width: Rc<dyn Fn() -> i64> = {
        let panel_fullscreen = panel_fullscreen.clone();
        let available_width = available_width.clone();
        Rc::new(move || if panel_fullscreen() { available_width() } else { (available_width() / 2).max(1) })
    };

    let wide: Rc<dyn Fn() -> bool> = {
        let vertical_tabs_width = vertical_tabs_width.clone();
        Rc::new(move || dimensions.get().0 as i64 - vertical_tabs_width() as i64 > 120)
    };
    let terminal_entries: Signal<Vec<Json>> = create_signal(Vec::new()).0;
    let selected_terminal: Signal<Option<String>> = create_signal(None).0;
    let terminal_focus: Signal<Option<String>> = create_signal(None).0;
    // `clampSessionPaneWidth(defaultPaneWidth(), panels.width())`.
    let terminal_width: Rc<dyn Fn() -> i64> = {
        let available_width = available_width.clone();
        Rc::new(move || {
            let total = available_width();
            let half = (total / 2).max(1);
            (24.min(half)).max(half.min((total - SESSION_CONTENT_MIN_WIDTH).max(half)))
        })
    };
    let selected_terminal_id: Rc<dyn Fn() -> Option<String>> = {
        Rc::new(move || {
            let selected = selected_terminal.get()?;
            terminal_entries.with(|list| list.iter().any(|terminal| terminal.str("id") == Some(selected.as_str()))).then_some(selected)
        })
    };
    let api = t.data.client_context.api();
    let directory_of = {
        let data = t.data.clone();
        let session_id = session_id.clone();
        Rc::new(move || {
            data.store
                .session
                .get(&session_id)
                .and_then(|session| session.get("location").and_then(|location| location.str("directory")).map(str::to_string))
        })
    };
    let refresh_terminals: Rc<dyn Fn()> = {
        let api = api.clone();
        let session_id = session_id.clone();
        Rc::new(move || {
            let (api, session_id) = (api.clone(), session_id.clone());
            tokio::task::spawn_local(async move {
                if let Ok(list) = list_terminals(&api, &session_id).await {
                    terminal_entries.set(list.clone());
                    if let Some(selected) = selected_terminal.get_untracked()
                        && !list.iter().any(|terminal| terminal.str("id") == Some(selected.as_str()))
                    {
                        selected_terminal.set(None);
                    }
                }
            });
        })
    };
    let select_terminal: Rc<dyn Fn(Option<String>, bool)> = {
        Rc::new(move |id: Option<String>, focus: bool| {
            terminal_focus.set(if focus { id.clone() } else { None });
            selected_terminal.set(id);
        })
    };
    let new_terminal: Rc<dyn Fn()> = {
        let api = api.clone();
        let session_id = session_id.clone();
        let directory_of = directory_of.clone();
        let select_terminal = select_terminal.clone();
        Rc::new(move || {
            let (api, session_id, directory_of) = (api.clone(), session_id.clone(), directory_of.clone());
            let select_terminal = select_terminal.clone();
            tokio::task::spawn_local(async move {
                let mut input = obj! {
                    "sessionID" => session_id.as_str(),
                    "args" => Json::Array(vec![]),
                    "title" => "Terminal",
                    "env" => Json::Object(Default::default()),
                };
                if let (Json::Object(object), Some(directory)) = (&mut input, directory_of()) {
                    object.insert("cwd".into(), Json::String(directory));
                }
                let Ok(value) = api.api().experimental().persistent_pty().create(&input, RequestOptions::default()).await else {
                    return;
                };
                let id = value.json().str("id").map(str::to_string);
                if let Ok(list) = list_terminals(&api, &session_id).await {
                    terminal_entries.set(list);
                }
                if let Some(id) = id {
                    select_terminal(Some(id), true);
                }
            });
        })
    };
    {
        // A terminal added or removed by any client keeps this session's list current.
        let (refresh, session_id) = (refresh_terminals.clone(), session_id.clone());
        let listener = t.data.store.listen(move |event| {
            if !matches!(event.r#type(), "persistent-pty.added" | "persistent-pty.removed" | "persistent-pty.updated") {
                return;
            }
            if event.data().str("sessionID") != Some(session_id.as_str()) {
                return;
            }
            if terminal_entries.with(Vec::is_empty) {
                return;
            }
            refresh();
        });
        on_cleanup(move || listener.call());
    }
    {
        let refresh = refresh_terminals.clone();
        on_mount(move || refresh());
    }
    let show_terminals: FocusCallback = Rc::new(RefCell::new(None));
    let focus_terminal: FocusCallback = Rc::new(RefCell::new(None));
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
        let panel_target = panel_target.clone();
        let selected_terminal_id = selected_terminal_id.clone();
        move || {
            if panel_target.get().is_some() {
                return RightPane::Panel;
            }
            if sidebar_open.get() && sidebar_visible() {
                return RightPane::Sidebar;
            }
            if selected_terminal_id().is_some() {
                return RightPane::Terminal;
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
        let focus_session = {
            let prompt = t.prompt.clone();
            Rc::new(move || {
                if let Some(prompt) = prompt.borrow().clone() {
                    prompt.focus();
                }
            })
        };
        let refresh = refresh_terminals.clone();
        let new_terminal = new_terminal.clone();
        let select_terminal = select_terminal.clone();
        let show_terminals = show_terminals.clone();
        let session_id_for_command = session_id.clone();
        create_layer(move || {
            let toggle = toggle.clone();
            let dialog = dialog.clone();
            let right_pane = right_pane.clone();
            let focus_session = focus_session.clone();
            let refresh = refresh.clone();
            let new_terminal = new_terminal.clone();
            let select_terminal = select_terminal.clone();
            let show_terminals = show_terminals.clone();
            crate::keymap::Layer {
                mode: Some("global"),
                commands: vec![
                    crate::keymap::Command::new("session.sidebar.toggle", {
                        let toggle = toggle.clone();
                        let dialog = dialog.clone();
                        move || {
                            toggle();
                            dialog.clear();
                        }
                    })
                    .title(if right_pane() == RightPane::Sidebar { "Hide sidebar" } else { "Show sidebar" })
                    .group("Session")
                    .palette(true),
                    Command::new("terminal.toggle", {
                        let dialog = dialog.clone();
                        let right_pane = right_pane.clone();
                        let focus_session = focus_session.clone();
                        let select_terminal = select_terminal.clone();
                        let api = api.clone();
                        let session_id = session_id_for_command.clone();
                        let directory_of = directory_of.clone();
                        let entries = terminal_entries;
                        let select = select_terminal.clone();
                        move || {
                            dialog.clear();
                            if right_pane() == RightPane::Terminal {
                                focus_session();
                                select_terminal(None, false);
                                return;
                            }
                            // The last terminal opens again; with none, the session gets one.
                            let (api, session_id, directory_of) = (api.clone(), session_id.clone(), directory_of.clone());
                            let select = select.clone();
                            tokio::task::spawn_local(async move {
                                let list = list_terminals(&api, &session_id).await.unwrap_or_default();
                                entries.set(list.clone());
                                if let Some(id) = list.last().and_then(|terminal| terminal.str("id")).map(str::to_string) {
                                    select(Some(id), true);
                                    return;
                                }
                                if let Ok(terminal) = create_terminal(&api, &session_id, directory_of()).await {
                                    if let Ok(list) = list_terminals(&api, &session_id).await {
                                        entries.set(list);
                                    }
                                    select(terminal.str("id").map(str::to_string), true);
                                }
                            });
                        }
                    })
                    .title(if right_pane() == RightPane::Terminal { "Hide terminal pane" } else { "Show terminal pane" })
                    .group("Session")
                    .palette(true),
                    Command::new("terminal.select", {
                        let dialog = dialog.clone();
                        let focus_session = focus_session.clone();
                        let refresh = refresh.clone();
                        let show_terminals = show_terminals.clone();
                        move || {
                            dialog.clear();
                            focus_session();
                            if let Some(show) = show_terminals.borrow().clone() {
                                show();
                            }
                            refresh();
                        }
                    })
                    .title("Select terminal")
                    .group("Session")
                    .palette(true),
                    Command::new("terminal.close", {
                        let dialog = dialog.clone();
                        let focus_session = focus_session.clone();
                        let select_terminal = select_terminal.clone();
                        move || {
                            dialog.clear();
                            focus_session();
                            select_terminal(None, false);
                        }
                    })
                    .title("Close terminal pane")
                    .group("Session")
                    .palette(true)
                    .enabled_fn({
                        let right_pane = right_pane.clone();
                        move || right_pane() == RightPane::Terminal
                    }),
                    Command::new("session.terminal", {
                        let dialog = dialog.clone();
                        let new_terminal = new_terminal.clone();
                        move || {
                            dialog.clear();
                            new_terminal();
                        }
                    })
                    .title("New terminal")
                    .group("Session")
                    .palette(true)
                    .bind(Bind::Config)
                    .slash("terminal", &[]),
                ],
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
    {
        let panel_fullscreen = panel_fullscreen.clone();
        bind_always(move || !panel_fullscreen(), move |visible| session_node.set_visible(visible));
    }
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
        let on_terminal_select = select_terminal.clone();
        let visible = selected_terminal_id.clone();
        let on_new = {
            let new_terminal = new_terminal.clone();
            Rc::new(move || new_terminal())
        };
        let show = show_terminals.clone();
        let on_picker = Rc::new(move |value: Option<Rc<dyn Fn()>>| {
            *show.borrow_mut() = value;
        });
        insert(
            session_node,
            component(move || {
                let sidebar_visible = sidebar_visible.clone();
                let on_toggle = on_toggle.clone();
                let vertical_tabs_width = vertical_tabs_width.clone();
                let on_terminal_select = on_terminal_select.clone();
                let visible = visible.clone();
                let on_picker = on_picker.clone();
                session(SessionProps {
                    scroll_ref: None,
                    vertical_tabs_width,
                    prompt_muted: Rc::new(|| false),
                    sidebar_visible: Rc::new(move || sidebar_visible()),
                    on_toggle_sidebar: on_toggle,
                    // `useSessionTerminals().available()`: the server runs the terminals.
                    terminals: Rc::new(|| true),
                    terminal_entries: Rc::new(move || terminal_entries.get()),
                    on_terminal_select: Some(Rc::new(move |id: Option<String>| on_terminal_select(id, true))),
                    on_terminal_new: Some(on_new),
                    visible_terminal_id: Rc::new(move || visible()),
                    on_terminal_picker: Some(on_picker),
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
                let wide = wide.clone();
                move || right_pane() == RightPane::Sidebar && wide()
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
    // The terminal pane: the session's persistent terminal beside the transcript.
    {
        let selected = selected_terminal_id.clone();
        let terminal_width = terminal_width.clone();
        let focus_terminal = focus_terminal.clone();
        insert(
            outer,
            show_keyed(
                move || selected(),
                move |pty_id| {
                    let focus_terminal = focus_terminal.clone();
                    let pane = box_();
                    pane.set_flex_shrink(Some(0.0));
                    pane.set_height(Some(Value::Percent(100.0)));
                    pane.set_min_width(Some(Value::Point(0.0)));
                    pane.set_min_height(Some(Value::Point(0.0)));
                    {
                        let terminal_width = terminal_width.clone();
                        bind(move || terminal_width() as f32, move |width| pane.set_width(Some(Value::Point(width))));
                    }
                    let id = pty_id.clone();
                    insert(
                        pane,
                        component(move || {
                            let id = id;
                            let focus_id = id.clone();
                            let focus_terminal = focus_terminal.clone();
                            terminal_pane(TerminalPaneProps {
                                pty_id: id.clone(),
                                resizing: Rc::new(|| false),
                                auto_focus: {
                                    let id = focus_id;
                                    Rc::new(move || terminal_focus.get().as_deref() == Some(id.as_str()))
                                },
                                on_auto_focus: Some(Rc::new(move || terminal_focus.set(None))),
                                on_focus_request: Some(Rc::new(move |value| {
                                    *focus_terminal.borrow_mut() = value;
                                })),
                                on_disconnect: None,
                            })
                        }),
                        At::Only,
                    );
                    View::Node(pane)
                },
                None,
            ),
            At::End,
        );
    }

    // The docked panel (`PanelHost` + the `session.panel` slot): the diff viewer beside the session.
    insert(
        outer,
        show(
            {
                let panel_target = panel_target.clone();
                move || panel_target.get().is_some()
            },
            {
                let session_id = session_id.clone();
                let panel_target = panel_target.clone();
                let panel_width = panel_width.clone();
                let panel_fullscreen = panel_fullscreen.clone();
                let t = tui();
                move || {
                    let Some(target) = panel_target.get() else { return View::Empty };
                    let pane = box_();
                    pane.set_flex_shrink(Some(0.0));
                    pane.set_min_width(Some(Value::Point(0.0)));
                    pane.set_min_height(Some(Value::Point(0.0)));
                    pane.set_height(Some(Value::Percent(100.0)));
                    {
                        let panel_width = panel_width.clone();
                        bind(move || panel_width() as f32, move |width| pane.set_width(Some(Value::Point(width))));
                    }
                    let input = crate::diff_viewer::PanelInput {
                        name: target.name.clone(),
                        session_id: target.session_id.clone(),
                        width: {
                            let panel_width = panel_width.clone();
                            Rc::new(move || panel_width())
                        },
                        docked: {
                            let panel_fullscreen = panel_fullscreen.clone();
                            Rc::new(move || !panel_fullscreen())
                        },
                        focused: Rc::new(|| true),
                        focus: Rc::new(|| {}),
                        close: {
                            let t = t.clone();
                            Rc::new(move || {
                                t.panel.set(None);
                                t.panel_fullscreen.set(false);
                            })
                        },
                        toggle_fullscreen: {
                            let t = t.clone();
                            Rc::new(move || {
                                t.panel_fullscreen.set(!t.panel_fullscreen.get_untracked());
                            })
                        },
                    };
                    let id = session_id.clone();
                    let on_close: Rc<dyn Fn()> = {
                        let t = t.clone();
                        Rc::new(move || {
                            t.panel.set(None);
                            t.panel_fullscreen.set(false);
                        })
                    };
                    insert(
                        pane,
                        component(move || crate::diff_viewer::diff_viewer(Some(id.clone()), None, Some(input.clone()), on_close.clone())),
                        At::Only,
                    );
                    View::Node(pane)
                }
            },
            None,
        ),
        At::End,
    );
    // The pane focus commands (`SessionFrame`): the diff viewer's footer shows the session pane's shortcut.
    {
        let prompt = t.prompt.clone();
        let focus_prompt = Rc::new(move || {
            if let Some(prompt) = prompt.borrow().clone() {
                prompt.focus();
            }
        });
        let focus_terminal = focus_terminal.clone();
        let focus_right = Rc::new(move || {
            if let Some(focus) = focus_terminal.borrow().clone() {
                focus();
            }
        });
        create_layer(move || {
            let focus_prompt = focus_prompt.clone();
            let focus_right = focus_right.clone();
            crate::keymap::Layer {
                commands: vec![
                    crate::keymap::Command::new("pane.focus.left", move || focus_prompt())
                        .title("Focus session pane")
                        .group("Session")
                        .palette(true),
                    crate::keymap::Command::new("pane.focus.right", move || focus_right())
                        .title("Focus right pane")
                        .group("Session")
                        .palette(true),
                ],
                ..crate::keymap::Layer::default()
            }
        });
    }
    View::Node(outer)
}

/// `persistentPty.list({ sessionID })`: the session's terminals.
async fn list_terminals(api: &Client, session_id: &str) -> Result<Vec<Json>, Error> {
    let value = api
        .api()
        .experimental()
        .persistent_pty()
        .list(&obj! { "sessionID" => session_id }, RequestOptions::default())
        .await?;
    Ok(match value.json() {
        Json::Array(items) => items,
        _ => Vec::new(),
    })
}

/// `sessions.newTerminal`: the session's shell in the daemon, in the session's directory.
async fn create_terminal(api: &Client, session_id: &str, directory: Option<String>) -> Result<Json, Error> {
    let mut input = obj! {
        "sessionID" => session_id,
        "args" => Json::Array(vec![]),
        "title" => "Terminal",
        "env" => Json::Object(Default::default()),
    };
    if let (Json::Object(object), Some(directory)) = (&mut input, directory) {
        object.insert("cwd".into(), Json::String(directory));
    }
    Ok(api.api().experimental().persistent_pty().create(&input, RequestOptions::default()).await?.json())
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
    insert(
        content,
        component({
            let session_id = session_id.clone();
            move || sidebar_todo(session_id.clone())
        }),
        At::End,
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
                                    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| tokens.set_fg(Some(c)));
                                    let state_tokens = state.clone();
                                    bind(
                                        move || state_tokens.get().map(|(tokens, _)| format!("{} tokens", grouped(tokens as i64))).unwrap_or_default(),
                                        move |value| tokens.set_content(&value),
                                    );
                                    let percent = text();
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
                    insert(
                        el,
                        show(
                            move || cost.get() > 0.0,
                            move || {
                                let spent = text();
                                bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| spent.set_fg(Some(c)));
                                bind(move || format!("{:.2} spent", cost.get()), move |value| spent.set_content(&value));
                                View::Node(spent)
                            },
                            None,
                        ),
                        At::End,
                    );
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

/// `SidebarTodo` (`feature-plugins/sidebar/todo.tsx`): the session's latest todo list.
fn sidebar_todo(session_id: String) -> View {
    let t = tui();
    let theme = t.theme;
    let data = t.data.clone();
    let list = {
        let data = data.clone();
        let session_id = session_id.clone();
        create_memo_equals(
            move || {
                let messages = data.store.session.message.list(&session_id);
                let boundary = data.store.session.get(&session_id).and_then(|s| match s.get("revert").and_then(|r| r.str("messageID")) {
                    Some(boundary) => Some(boundary.to_string()),
                    None => None,
                });
                crate::tools::todo::session_todos(&messages, boundary.as_deref())
            },
            None,
        )
    };
    let (open, set_open) = create_signal(true);
    let root = box_();
    insert(
        root,
        show(
            move || !list.get().is_empty(),
            {
                let list = list.clone();
                move || {
                    let el = box_();
                    el.set_flex_direction(FlexDirection::Column);
                    let heading = box_();
                    insert_node(el, heading, None);
                    heading.set_flex_direction(FlexDirection::Row);
                    heading.set_gap(2, Some(Value::Point(1.0)));
                    {
                        let list = list.clone();
                        let set_open = set_open.clone();
                        let opened = open.clone();
                        heading.set_on_mouse(
                            Some(opencode_tui_core::tree::MouseEventType::Down),
                            Some(Rc::new(move |_| {
                                if list.get().len() <= 2 {
                                    return;
                                }
                                set_open.set(!opened.get());
                            })),
                        );
                    }
                    let marker = text();
                    insert_node(heading, marker, None);
                    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| marker.set_fg(Some(c)));
                    insert(
                        marker,
                        show(move || list.get().len() > 2, move || View::from(if open.get() { "▼" } else { "▶" }), None),
                        At::Only,
                    );
                    let label = text();
                    insert_node(heading, label, None);
                    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| label.set_fg(Some(c)));
                    label.set_attributes(Some(opencode_tui_core::color::text_attributes::BOLD));
                    bind(
                        {
                            let list = list.clone();
                            move || {
                                if list.get().len() <= 2 || open.get() {
                                    return "Todo".to_string();
                                }
                                let remaining = list.get().iter().filter(|todo| {
                                    matches!(todo.status, crate::tools::todo::TodoStatus::Pending | crate::tools::todo::TodoStatus::InProgress)
                                }).count();
                                format!("Todo ({remaining} remaining)")
                            }
                        },
                        move |value| label.set_content(&value),
                    );
                    insert(
                        el,
                        show(
                            {
                                let list = list.clone();
                                move || list.get().len() <= 2 || open.get()
                            },
                            {
                                let list = list.clone();
                                let theme = theme.clone();
                                move || {
                                    let theme = theme.clone();
                                    for_each(
                                        move || list.get(),
                                        |todo| todo.content.clone(),
                                        move |todo, _| {
                                            let theme = theme.clone();
                                            crate::tools::todo::todo_item(theme, Rc::new(move || todo.get()))
                                        },
                                        None,
                                    )
                                }
                            },
                            None,
                        ),
                        At::End,
                    );
                    View::Node(el)
                }
            },
            None,
        ),
        At::Only,
    );
    View::Node(root)
}
