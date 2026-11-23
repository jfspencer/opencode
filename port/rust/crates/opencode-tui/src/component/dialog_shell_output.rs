//! `component/dialog-shell-output.tsx`: a running or finished shell command's output, polled while the viewer is open.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, scrollbox, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::scrollbox::{ScrollBarOptions, StickyStart};
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::Edge;
use opencode_tui_sync::json::JsonExt;

use crate::ctx::tui;
use crate::keymap::{Command, Layer, create_layer};
use crate::theme::SurfaceName;
use crate::ui::dialog::{
    DialogSize, FooterAction, FooterActions, FooterProps, HeaderProps, LayoutProps, dialog_footer, dialog_header, dialog_layout,
    use_dialog, use_new_designs,
};
use crate::ui::theme::use_theme;
use crate::util::rgba;

const PAGE_BYTES: usize = 64 * 1024;

/// The poll step (a late-bound closure that schedules the next one).
type Step = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// `TextBoundary.tail(text, size)`: the last `size` characters.
fn tail(text: &str, size: usize) -> String {
    let count = text.chars().count();
    if count <= size {
        return text.to_string();
    }
    text.chars().skip(count - size).collect()
}

/// `<DialogShellOutput shell location>`.
pub fn dialog_shell_output(shell: Rc<Json>, directory: Option<String>) -> View {
    let dialog = use_dialog();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let dimensions = tui().dimensions;
    let client = tui().data.client.clone();
    let toast = tui().toast.clone();
    let new_designs = use_new_designs();
    let id = shell.str("id").unwrap_or_default().to_string();
    let command = shell.str("command").unwrap_or_default().to_string();
    let (info, set_info) = create_signal(shell.clone());
    let (output, set_output) = create_signal(None::<String>);
    let (omitted, set_omitted) = create_signal(false);
    let (error, set_error) = create_signal(String::new());
    let (copied, set_copied) = create_signal(None::<&'static str>);
    let height = create_memo(move || (dimensions.get().1 as f32 * 0.6).floor().max(3.0) - 6.0);
    let plain_text = create_memo(move || {
        let value = output.get().unwrap_or_default();
        crate::tools::display::strip_ansi(&value).replace("\r\n", "\n").replace('\r', "\n")
    });
    let scroll: Rc<Cell<Option<opencode_tui_core::tree::Element>>> = Rc::new(Cell::new(None));

    dialog.set_size(DialogSize::Xlarge);
    dialog.set_centered(true);

    {
        // Keep the view tied to the opened ID and its original Location, not the list's current selection.
        let location = directory.clone();
        let client = client.clone();
        let alive = Rc::new(Cell::new(true));
        let timer: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
        let cursor: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));
        let loading = create_signal(false).0;
        let step: Step = Rc::new(RefCell::new(None));
        let schedule = {
            let (alive, timer, step) = (alive.clone(), timer.clone(), step.clone());
            Rc::new(move |delay: u64| {
                let (alive, step) = (alive.clone(), step.clone());
                let handle = renderer::set_timeout(delay, move || {
                    if !alive.get() {
                        return;
                    }
                    if let Some(step) = step.borrow().clone() {
                        step();
                    }
                });
                timer.set(Some(handle));
            })
        };
        {
            let (alive, cursor, loading, schedule) =
                (alive.clone(), cursor.clone(), loading, schedule.clone());
            let (id, location, client) = (id.clone(), location.clone(), client);
            *step.borrow_mut() = Some(Rc::new(move || {
                if loading.get() || !alive.get() {
                    return;
                }
                loading.set(true);
                let (alive, cursor, schedule) = (alive.clone(), cursor.clone(), schedule.clone());
                let (id, location, client) = (id.clone(), location.clone(), client.clone());
                tokio::task::spawn_local(async move {
                    let mut input = opencode_tui_sync::obj! { "id" => id.as_str() };
                    if let Some(directory) = location.as_deref().filter(|d| !d.is_empty()) {
                        input.set("location", opencode_tui_sync::obj! { "directory" => directory });
                    }
                    let read = |input: Json| client.call_named("shell.output", &input, Default::default());
                    let running = untrack(|| info.get()).str("status") == Some("running");
                    if running {
                        let mut get = opencode_tui_sync::obj! { "id" => id.as_str() };
                        if let Some(directory) = location.as_deref().filter(|d| !d.is_empty()) {
                            get.set("location", opencode_tui_sync::obj! { "directory" => directory });
                        }
                        if let Ok(response) = client.call_named("shell.get", &get, Default::default()).await {
                            let data = response.json().get("data").cloned().unwrap_or(Json::Null);
                            if !matches!(data, Json::Null) {
                                set_info.set(Rc::new(data));
                            }
                        }
                    }
                    if cursor.get().is_none() {
                        let mut head = input.clone();
                        head.set("cursor", Json::Number(f64::MAX));
                        if let Ok(response) = read(head).await {
                            let data = response.json().get("data").cloned().unwrap_or(Json::Null);
                            let size = data.num("size").unwrap_or(0.0).max(0.0);
                            let start = (size - PAGE_BYTES as f64).max(0.0);
                            cursor.set(Some(start));
                            set_omitted.set(start > 0.0);
                        }
                    }
                    let mut page = input;
                    page.set("cursor", Json::Number(cursor.get().unwrap_or(0.0)));
                    page.set("limit", Json::Number(PAGE_BYTES as f64));
                    match read(page).await {
                        Ok(response) => {
                            let data = response.json().get("data").cloned().unwrap_or(Json::Null);
                            let chunk = data.str("output").unwrap_or_default().to_string();
                            let next_cursor = data.num("cursor").unwrap_or(0.0);
                            let size = data.num("size").unwrap_or(0.0);
                            let advanced = next_cursor > cursor.get().unwrap_or(0.0);
                            cursor.set(Some(next_cursor));
                            set_output.update(|previous| {
                                let next = previous.clone().unwrap_or_default() + &chunk;
                                if next.chars().count() > PAGE_BYTES {
                                    set_omitted.set(true);
                                }
                                *previous = Some(tail(&next, PAGE_BYTES));
                            });
                            set_error.set(String::new());
                            loading.set(false);
                            if alive.get() {
                                let more = advanced && next_cursor < size;
                                schedule(if more { 0 } else { 1_000 });
                            }
                            return;
                        }
                        Err(cause) => {
                            let missing = cause.message().to_lowercase().contains("not found");
                            set_error.set(if missing {
                                "Shell output is no longer available.".to_string()
                            } else {
                                "Unable to read shell output. Retrying…".to_string()
                            });
                        }
                    }
                    loading.set(false);
                    if alive.get() {
                        schedule(1_000);
                    }
                });
            }));
        }
        // Kick off the poll once the view is mounted.
        renderer::set_timeout(0, {
            let step = step.clone();
            move || {
                if let Some(step) = step.borrow().clone() {
                    step();
                }
            }
        });
        on_cleanup({
            let (alive, timer, step) = (alive, timer, step);
            move || {
                alive.set(false);
                if let Some(handle) = timer.take() {
                    renderer::clear_timeout(handle);
                }
                // Break the step → schedule → step cycle.
                *step.borrow_mut() = None;
            }
        });
    }

    let status = move || match untrack(|| info.get()).str("status") {
        Some("running") => "Running".to_string(),
        Some("timeout") => "Timed out".to_string(),
        Some("killed") => "Killed".to_string(),
        _ => match untrack(|| info.get()).get("exit").and_then(|e| match e {
            Json::Number(n) => Some(*n),
            _ => None,
        }) {
            Some(code) => format!("Exited · code {code}"),
            None => "Exited".to_string(),
        },
    };
    let copy = {
        let (command, plain_text, toast) = (command.clone(), plain_text.clone(), toast.clone());
        Rc::new(move |kind: &'static str| {
            let value = if kind == "command" { command.clone() } else { plain_text.with(|t| t.clone()) };
            if value.is_empty() {
                return;
            }
            let toast = toast.clone();
            tokio::task::spawn_local(async move {
                match crate::selection::write_clipboard(&value).await {
                    Ok(()) => set_copied.set(Some(kind)),
                    Err(_) => toast.error("Failed to copy to clipboard"),
                }
            });
        })
    };
    {
        let copy = copy.clone();
        let scroll = scroll.clone();
        let height = height.clone();
        create_layer(move || {
            let new_designs = new_designs();
            let mut commands = Vec::new();
            if new_designs {
                let copy_command = copy.clone();
                commands.push(Command::inline("c", move || {
                    copy_command("command");
                    true
                }).title("Copy command").group("Shell"));
                let copy_output = copy.clone();
                commands.push(Command::inline("o", move || {
                    copy_output("output");
                    true
                }).title("Copy output").group("Shell"));
            }
            let up = scroll.clone();
            commands.push(Command::inline("up", move || {
                if let Some(scroll) = up.get() {
                    scroll.scroll_by(-1.0, opencode_tui_core::renderables::scrollbox::ScrollUnit::Absolute);
                }
                true
            }).title("Scroll output up").group("Shell"));
            let down = scroll.clone();
            commands.push(Command::inline("down", move || {
                if let Some(scroll) = down.get() {
                    scroll.scroll_by(1.0, opencode_tui_core::renderables::scrollbox::ScrollUnit::Absolute);
                }
                true
            }).title("Scroll output down").group("Shell"));
            let pageup = scroll.clone();
            let page_height = height.clone();
            commands.push(Command::inline("pageup", move || {
                if let Some(scroll) = pageup.get() {
                    scroll.scroll_by(-(page_height.get() as f64), opencode_tui_core::renderables::scrollbox::ScrollUnit::Absolute);
                }
                true
            }).title("Previous output page").group("Shell"));
            let pagedown = scroll.clone();
            commands.push(Command::inline("pagedown", move || {
                if let Some(scroll) = pagedown.get() {
                    scroll.scroll_by(page_height.get() as f64, opencode_tui_core::renderables::scrollbox::ScrollUnit::Absolute);
                }
                true
            }).title("Next output page").group("Shell"));
            let home = scroll.clone();
            commands.push(Command::inline("home", move || {
                if let Some(scroll) = home.get() {
                    scroll.scroll_to(0.0);
                }
                true
            }).title("First loaded output").group("Shell"));
            let end = scroll.clone();
            commands.push(Command::inline("end", move || {
                if let Some(scroll) = end.get() {
                    scroll.scroll_to(f64::INFINITY);
                }
                true
            }).title("Follow shell output").group("Shell"));
            Layer { mode: Some("modal"), commands, ..Layer::default() }
        });
    }
    let follow = {
        let scroll = scroll.clone();
        Rc::new(move || {
            if let Some(scroll) = scroll.get() {
                scroll.scroll_to(f64::INFINITY);
            }
        })
    };
    let copy_action = {
        let (copy, copied) = (copy.clone(), copied);
        move |kind: &'static str| -> FooterAction {
            let on_trigger: Rc<dyn Fn()> = {
                let copy = copy.clone();
                Rc::new(move || copy(kind))
            };
            if copied.get() == Some(kind) {
                return FooterAction {
                    title: "✓ copied".into(),
                    color: Some(rgba(theme.get().text.feedback.success.base)),
                    on_trigger: Some(on_trigger),
                    ..FooterAction::default()
                };
            }
            FooterAction {
                title: format!("copy {kind}"),
                key: Some(if kind == "command" { "c" } else { "o" }.into()),
                on_trigger: Some(on_trigger),
                ..FooterAction::default()
            }
        }
    };
    let footer = {
        let (follow, copy_action) = (follow.clone(), copy_action.clone());
        FooterProps {
            actions: Some(Rc::new(move || FooterActions {
                primary: vec![
                    FooterAction { title: "follow".into(), key: Some("end".into()), on_trigger: Some(follow.clone()), ..FooterAction::default() },
                    copy_action("command"),
                    copy_action("output"),
                ],
                ..FooterActions::default()
            })),
            ..FooterProps::default()
        }
    };
    let legacy_footer = {
        let follow = follow.clone();
        Rc::new(move || {
            let follow = follow.clone();
            dialog_footer(FooterProps {
                actions: Some(Rc::new(move || FooterActions {
                    secondary: vec![
                        FooterAction { title: "scroll".into(), key: Some("↑/↓".into()), ..FooterAction::default() },
                        FooterAction { title: "follow".into(), key: Some("end".into()), on_trigger: Some(follow.clone()), ..FooterAction::default() },
                        FooterAction { title: "back".into(), key: Some("esc".into()), ..FooterAction::default() },
                    ],
                    ..FooterActions::default()
                })),
                ..FooterProps::default()
            })
        })
    };

    let header = {
        let status = status.clone();
        dialog_header(HeaderProps {
            title_view: Some(Rc::new(move || {
                let el = text();
                el.set_flex_grow(Some(1.0));
                el.set_attributes(Some(text_attributes::BOLD));
                bind_always(move || rgba(theme.get().text.base), move |c| el.set_fg(Some(c)));
                insert(el, View::from("Shell output"), At::Only);
                View::Node(el)
            })),
            children: Some(Rc::new(move || {
                let el = text();
                bind_always(move || rgba(theme.get().text.muted), move |c| el.set_fg(Some(c)));
                insert(el, View::dynamic(move || View::from(status())), At::Only);
                View::Node(el)
            })),
            gap: Some(if dimensions.get_untracked().0 < 60 { 1.0 } else { 2.0 }),
            ..HeaderProps::default()
        })
    };

    dialog_layout(LayoutProps {
        header: Box::new(|| header),
        footer: Some(footer),
        legacy_footer: Some(legacy_footer),
        children: Box::new(move || {
            let outer = box_();
            outer.set_gap(2, Some(Value::Point(1.0)));
            {
                let el = text();
                el.set_max_height(Some(Value::Point(3.0)));
                el.set_wrap_mode(WrapMode::Word);
                bind_always(move || rgba(theme.get().text.muted), move |c| el.set_fg(Some(c)));
                insert(el, View::from(command.clone()), At::Only);
                insert_node(outer, el, None);
            }
            insert(
                outer,
                show(
                    move || omitted.get(),
                    move || {
                        let el = text();
                        bind_always(move || rgba(theme.get().text.muted), move |c| el.set_fg(Some(c)));
                        insert(el, View::from("Earlier output omitted · showing recent output"), At::Only);
                        View::Node(el)
                    },
                    None,
                ),
                At::End,
            );
            let el = scrollbox();
            el.set_id("shell-output-scroll");
            scroll.set(Some(el));
            el.set_sticky_scroll(true);
            el.set_sticky_start(Some(StickyStart::Bottom));
            el.set_scrollbar_options(None, &ScrollBarOptions { visible: Some(Some(false)), ..ScrollBarOptions::default() });
            bind(move || height.get(), move |h| el.set_height(Some(Value::Point(h))));
            let body = text();
            body.set_wrap_mode(WrapMode::Word);
            bind_always(move || rgba(theme.get().text.base), move |c| body.set_fg(Some(c)));
            insert(
                body,
                View::dynamic(move || {
                    let value = plain_text.get();
                    if !value.is_empty() {
                        return View::from(value);
                    }
                    if output.get().is_none() {
                        return View::from("Loading output…");
                    }
                    View::from("No captured output. Output redirected to files is not shown here.")
                }),
                At::Only,
            );
            insert_node(el, body, None);
            insert_node(outer, el, None);
            insert(
                outer,
                show(
                    move || !error.get().is_empty(),
                    {
                        let theme = theme;
                        move || {
                            let el = text();
                            bind_always(move || rgba(theme.get().text.feedback.error.base), move |c| el.set_fg(Some(c)));
                            insert(el, View::dynamic(move || View::from(error.get())), At::Only);
                            View::Node(el)
                        }
                    },
                    None,
                ),
                At::End,
            );
            outer.set_padding(Edge::Top, Some(Value::Point(0.0)));
            View::Node(outer)
        }),
        ..LayoutProps::default()
    })
}
