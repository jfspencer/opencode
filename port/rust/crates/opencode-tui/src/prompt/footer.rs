//! The built-in prompt feature plugins (`feature-plugins/prompt/`): the footer (`footer.tsx`, appended to the
//! `prompt.footer` slot: live subagents and shells, the goal, usage and cost, the shortcut hints, the shell mode
//! hint), the goal (`goal.tsx`: `createGoal` over the goal plugin's RPC, `GoalStatus`), `/btw` (`btw.tsx`), and the
//! running session's status in `prompt.footer.status` (`PromptInterruptStatus` and its spinner).
//!
//! TODO(port-tui): the goal dialog and `/btw`'s question and answer dialogs (the dialogs slice), the spinner's
//! per-cell trail colors (`createColors`).
use std::cell::Cell;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection, Justify, MouseEventType};
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::{Route, tui};
use crate::data::array;
use crate::keymap::{self, Command, Layer, Shortcuts, create_layer};
use crate::toast::{ToastInput, Variant};
use crate::util::{rgba, string_width};

fn num(value: &Json) -> Option<f64> {
    match value {
        Json::Number(n) => Some(*n),
        _ => None,
    }
}

fn string(value: &Json) -> Option<&str> {
    match value {
        Json::String(s) => Some(s),
        _ => None,
    }
}

pub struct FooterProps {
    pub session_id: Option<String>,
    pub shell: Rc<dyn Fn() -> bool>,
    pub show_details: Rc<dyn Fn() -> bool>,
    pub shortcuts: Shortcuts,
}

/// `Locale.number(num)`.
fn locale_number(num: f64) -> String {
    if num >= 999_950.0 {
        return format!("{:.1}M", num / 1_000_000.0);
    }
    if num >= 1000.0 {
        return format!("{:.1}K", num / 1000.0);
    }
    if num.fract() == 0.0 { format!("{}", num as i64) } else { num.to_string() }
}

/// `money.format(cost)` (`Intl.NumberFormat("en-US", { style: "currency", currency: "USD" })`).
fn money(cost: f64) -> String {
    let cents = (cost * 100.0).round() as i64;
    let dollars = cents / 100;
    let mut grouped = String::new();
    let digits = dollars.to_string();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("${grouped}.{:02}", cents % 100)
}

/// `contextUsage(messages, models, boundary)` and `formatContextUsage(tokens, percent)`.
fn context_usage(messages: &[Rc<Json>], models: Option<&Json>, boundary: Option<&str>) -> Option<String> {
    let index = opencode_tui_sync::usage::last_assistant_with_usage(messages, boundary)?;
    let last = &messages[index];
    let tokens = last.get("tokens")?;
    let n = |path: &str| tokens.at(path).and_then(num).unwrap_or(0.0);
    let total = n("input") + n("output") + n("reasoning") + n("cache.read") + n("cache.write");
    if total <= 0.0 {
        return None;
    }
    let provider = last.at("model.providerID").and_then(string);
    let id = last.at("model.id").and_then(string);
    let context = models.map(array).unwrap_or(&[]).iter().find(|m| m.str("providerID") == provider && m.str("id") == id).and_then(|m| {
        m.at("limit.context").and_then(num).filter(|c| *c != 0.0)
    });
    let value = locale_number(total);
    Some(match context {
        Some(context) => format!("{value} ({}%)", ((total / context) * 100.0).round() as i64),
        None => value,
    })
}

/// `promptFooterLayout({ width, usage, shortcuts })`: (usage shown, shortcuts shown).
fn prompt_footer_layout(width: i64, usage: &[String], shortcuts: &[String]) -> (bool, bool) {
    let usage = usage.join(" · ");
    let shortcuts = shortcuts.join(" · ");
    let available = (width - 28.min(width / 2)).max(0);
    if !usage.is_empty() && !shortcuts.is_empty() && string_width(&format!("{usage} · {shortcuts}")) as i64 <= available {
        return (true, true);
    }
    if !usage.is_empty() && string_width(&usage) as i64 <= available {
        return (true, false);
    }
    if usage.is_empty() && !shortcuts.is_empty() && string_width(&shortcuts) as i64 <= available {
        return (false, true);
    }
    (false, false)
}

/// `Goal.State`: what the footer and the dialog read.
#[derive(Clone, Debug, PartialEq)]
pub struct Goal {
    pub objective: String,
    pub status: String,
    pub reason: Option<String>,
}

fn goal_of(value: &Json) -> Option<Goal> {
    if matches!(value, Json::Null) {
        return None;
    }
    Some(Goal {
        objective: value.str("objective").unwrap_or_default().to_string(),
        status: value.str("status").unwrap_or_default().to_string(),
        reason: value.str("reason").map(str::to_string),
    })
}

/// `createGoal(context, sessionID)`: the session's goal from the goal plugin's RPC, fetched when the session changes
/// or the server reconnects, kept current from its `changed` events.
pub fn create_goal(session_id: Option<String>) -> Signal<Option<Goal>> {
    let t = tui();
    let goal = create_signal(None::<Goal>).0;
    let turn = Rc::new(Cell::new(0u64));
    let load = {
        let t = t.clone();
        let turn = turn.clone();
        Rc::new(move |id: String| {
            turn.set(turn.get() + 1);
            let current = turn.get();
            let directory = t.data.store.session.get(&id).and_then(|s| s.at("location.directory").and_then(string).map(str::to_string));
            let mut input = obj! { "rpcID" => "opencode.goal", "method" => "get", "input" => obj! { "sessionID" => id.as_str() } };
            if let Some(directory) = directory {
                input.set("location", obj! { "directory" => directory });
            }
            let call = t.data.client.call_named("rpc.call", &input, RequestOptions::default());
            let turn = turn.clone();
            tokio::task::spawn_local(async move {
                // A server without the goal plugin answers with an error, and the session shows no goal.
                if let Ok(value) = call.await
                    && current == turn.get()
                    && goal.is_alive()
                {
                    goal.set(goal_of(&value.json()));
                }
            });
        })
    };
    {
        let session = session_id.clone();
        let turn = turn.clone();
        let off = t.data.store.on("rpc.opencode.goal.changed", move |event| {
            if event.data().str("sessionID") != session.as_deref() {
                return;
            }
            turn.set(turn.get() + 1);
            if goal.is_alive() {
                goal.set(event.data().get("goal").and_then(goal_of));
            }
        });
        on_cleanup(move || off.call());
    }
    {
        let session = session_id.clone();
        let load = load.clone();
        let off = t.data.store.on("server.connected", move |_| {
            if let Some(id) = session.clone() {
                load(id);
            }
        });
        on_cleanup(move || off.call());
    }
    {
        let session = session_id.clone();
        create_effect(on(
            move || session.clone(),
            move |id: &Option<String>, _| {
                goal.set(None);
                if let Some(id) = id.clone() {
                    load(id);
                }
            },
            false,
        ));
    }
    goal
}

/// `<GoalStatus>`.
fn goal_status(goal: Signal<Option<Goal>>) -> View {
    let t = tui();
    let theme = t.theme;
    let toast = t.toast.clone();
    let hovered = create_signal(false).0;
    show_value(
        move || goal.get(),
        move |current| {
            let el = box_();
            el.set_flex_shrink(Some(0.0));
            el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| hovered.set(true))));
            el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| hovered.set(false))));
            let toast = toast.clone();
            el.set_on_mouse(
                Some(MouseEventType::Up),
                Some(Rc::new(move |_| {
                    if goal.get_untracked().is_none() {
                        toast.show(ToastInput::new(Variant::Info, "No goal is set. Start one with /goal <objective>."));
                    }
                    // TODO(port-tui): the dialogs slice: `GoalDialog`.
                })),
            );
            let label = text();
            insert_node(el, label, None);
            label.set_wrap_mode(WrapMode::None);
            bind_always(
                move || {
                    if hovered.get() {
                        return rgba(theme.with(|th| th.text.base));
                    }
                    match current.with(|g| g.as_ref().map(|g| g.status.clone())).as_deref() {
                        Some("paused") => rgba(theme.with(|th| th.text.feedback.warning.base)),
                        Some("complete") => rgba(theme.with(|th| th.text.feedback.success.base)),
                        _ => rgba(theme.with(|th| th.text.muted)),
                    }
                },
                move |c| label.set_fg(Some(c)),
            );
            insert(
                label,
                View::dynamic(move || {
                    let status = current.with(|g| g.as_ref().map(|g| g.status.clone())).unwrap_or_default();
                    View::from(format!(
                        "◎ {}",
                        match status.as_str() {
                            "paused" => "goal paused",
                            "complete" => "goal met",
                            _ => "goal",
                        }
                    ))
                }),
                At::Only,
            );
            View::Node(el)
        },
        None,
    )
}

/// `PromptFooter`.
pub fn prompt_footer(props: FooterProps) -> View {
    let t = tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let store = t.data.store.clone();
    let shortcuts = props.shortcuts;
    let session_id = props.session_id.clone();
    let live_hovered = create_signal(false).0;
    let subagents = {
        let store = store.clone();
        let session = session_id.clone();
        create_memo(move || {
            let id = session.as_deref()?;
            let count = store
                .session
                .family(id)
                .iter()
                .filter(|other| other.as_str() != id && store.session.status(other).as_str() == "running")
                .count();
            (count > 0).then(|| format!("{count} subagent{}", if count == 1 { "" } else { "s" }))
        })
    };
    let shells = {
        let store = store.clone();
        let session = session_id.clone();
        let t = t.clone();
        create_memo(move || {
            let id = session.as_deref()?;
            let location = t.data.directory.get().map(|d| obj! { "directory" => d });
            let count = store
                .shell
                .list(location.as_ref())
                .iter()
                .filter(|shell| shell.at("metadata.sessionID").and_then(string) == Some(id))
                .count();
            (count > 0).then(|| format!("{count} shell{}", if count == 1 { "" } else { "s" }))
        })
    };
    let status = {
        let store = store.clone();
        let session = session_id.clone();
        create_memo(move || {
            let Some(id) = session.as_deref() else { return Vec::<String>::new() };
            let Some(info) = store.session.get(id) else { return vec![] };
            let messages = store.session.message.list(id);
            let location = info.get("location").cloned();
            let models = store.location.model.list(location.as_ref());
            let boundary = info.at("revert.messageID").and_then(string).map(str::to_string);
            let usage = context_usage(&messages, models.as_deref(), boundary.as_deref());
            let cost = store.session.cost(id);
            [usage, (cost > 0.0).then(|| money(cost))].into_iter().flatten().collect()
        })
    };
    let live = create_memo(move || subagents.with(|s| s.is_some()) || shells.with(|s| s.is_some()));
    let goal = create_goal(session_id.clone());
    let interactive = crate::interactivity::use_interactivity();
    let layout = create_memo(move || {
        let command = shortcut(shortcuts, "command.palette.show");
        if status.with(|s| s.is_empty()) {
            return (false, dimensions.get().0 >= 44);
        }
        let shortcuts: Vec<String> = command.map(|c| format!("{c} commands")).into_iter().collect();
        status.with(|usage| prompt_footer_layout((dimensions.get().0 as i64 - 8).max(0), usage, &shortcuts))
    });

    let normal = {
        let props_details = props.show_details.clone();
        let interactive = interactive.clone();
        let session_id = session_id.clone();
        move || {
            let details = props_details.clone();
            let interactive_hint = interactive.clone();
            let session = session_id.clone();
            let status_view = {
                let details = details.clone();
                move || {
                    let row = box_();
                    row.set_flex_direction(FlexDirection::Row);
                    {
                        let details = details.clone();
                        bind(move || if details() && layout.get().0 { 0.0 } else { 1.0 }, move |s| row.set_flex_shrink(Some(s)));
                    }
                    row.set_min_width(Some(Value::Point(0.0)));
                    let live_view = show(
                        move || live.get(),
                        move || {
                            let el = box_();
                            el.set_flex_shrink(Some(0.0));
                            el.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| live_hovered.set(true))));
                            el.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| live_hovered.set(false))));
                            el.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(|_| keymap::dispatch("session.child.first", None))));
                            let label = text();
                            insert_node(el, label, None);
                            bind_always(
                                move || if live_hovered.get() { rgba(theme.with(|th| th.text.base)) } else { rgba(theme.with(|th| th.text.muted)) },
                                move |c| label.set_fg(Some(c)),
                            );
                            label.set_wrap_mode(WrapMode::None);
                            let key = show_value(
                                move || shortcut(shortcuts, "session.child.first"),
                                move |value| {
                                    let s = span();
                                    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| s.set_span_style(Some(c), None, 0, 0));
                                    insert(s, View::dynamic(move || View::from(format!("{} ", value.get().unwrap_or_default()))), At::Only);
                                    View::Node(s)
                                },
                                None,
                            );
                            insert(label, key, At::End);
                            insert(label, View::dynamic(move || View::from(subagents.get().unwrap_or_default())), At::End);
                            insert(
                                label,
                                View::dynamic(move || View::from(if subagents.with(|s| s.is_some()) && shells.with(|s| s.is_some()) { " · " } else { "" })),
                                At::End,
                            );
                            insert(label, View::dynamic(move || View::from(shells.get().unwrap_or_default())), At::End);
                            View::Node(el)
                        },
                        None,
                    );
                    insert(row, live_view, At::End);
                    let session = session.clone();
                    let goal_view = show(
                        move || session.is_some() && goal.with(|g| g.is_some()),
                        move || {
                            let separator = show(
                                move || live.get(),
                                move || {
                                    let el = text();
                                    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| el.set_fg(Some(c)));
                                    el.set_wrap_mode(WrapMode::None);
                                    el.set_flex_shrink(Some(0.0));
                                    insert(el, View::from(" · "), At::Only);
                                    View::Node(el)
                                },
                                None,
                            );
                            View::list([separator, goal_status(goal)])
                        },
                        None,
                    );
                    insert(row, goal_view, At::End);
                    let usage_view = {
                        let details = details.clone();
                        show(
                            move || details() && layout.get().0 && status.with(|s| !s.is_empty()),
                            move || {
                                let el = text();
                                bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| el.set_fg(Some(c)));
                                el.set_wrap_mode(WrapMode::None);
                                el.set_flex_shrink(Some(0.0));
                                insert(
                                    el,
                                    View::dynamic(move || {
                                        let prefix = if live.get() || goal.with(|g| g.is_some()) { " · " } else { "" };
                                        View::from(format!("{prefix}{}", status.get().join(" · ")))
                                    }),
                                    At::Only,
                                );
                                View::Node(el)
                            },
                            None,
                        )
                    };
                    insert(row, usage_view, At::End);
                    View::Node(row)
                }
            };
            let agents_hint = {
                let interactive = interactive_hint.clone();
                move || hint(shortcut(shortcuts, "agent.cycle").unwrap_or_default(), "agents", interactive.clone(), false)
            };
            let details_for_switch = details.clone();
            let first = switch(
                vec![
                    when(move || goal.with(|g| g.is_some()) || live.get() || status.with(|s| !s.is_empty()), status_view),
                    when(move || details_for_switch() && layout.get().1, agents_hint),
                ],
                None,
            );
            let interactive = interactive_hint.clone();
            let commands_hint = show(
                move || details() && layout.get().1,
                move || hint(shortcut(shortcuts, "command.palette.show").unwrap_or_default(), "commands", interactive.clone(), true),
                None,
            );
            View::list([first, commands_hint])
        }
    };
    let shell_hint = move || {
        let el = text();
        bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| el.set_fg(Some(c)));
        el.set_flex_shrink(Some(0.0));
        insert(el, View::from("esc "), At::End);
        let s = span();
        insert_node(el, s, None);
        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| s.set_span_style(Some(c), None, 0, 0));
        insert(s, View::dynamic(move || View::from(if dimensions.get().0 < 44 { "shell" } else { "exit shell mode" })), At::Only);
        View::Node(el)
    };
    let shell = props.shell.clone();
    let shell_when = props.shell.clone();
    switch(vec![when(move || !shell(), normal), when(move || shell_when(), shell_hint)], None)
}

/// `keymap.shortcuts(id)[0]`. TODO(port-tui): until the app registers `agent.cycle` and `command.palette.show`
/// (and the session route `session.child.first`), an unregistered command's hint is its configured binding as
/// written (`<leader>` as `ctrl+x `).
fn shortcut(shortcuts: Shortcuts, id: &str) -> Option<String> {
    if let Some(key) = shortcuts.get(id) {
        return Some(key);
    }
    let config = tui().config.clone();
    let binding = config.keybinds.get(id).first()?.clone();
    let opencode_protocol::Json::String(key) = binding.key() else { return None };
    let first = key.split(',').next()?.trim();
    Some(first.replace("<leader>", "ctrl+x "))
}

/// A shortcut hint (`{key} <span muted>{label}</span>`), dimmed while the prompt does not own input.
fn hint(key: String, label: &'static str, interactive: crate::interactivity::Interactive, no_wrap: bool) -> View {
    let theme = tui().theme;
    let el = text();
    {
        let interactive = interactive.clone();
        bind_always(
            move || {
                if interactive() { rgba(theme.with(|th| th.text.base)) } else { rgba(theme.with(|th| th.text.action.primary.disabled)) }
            },
            move |c| el.set_fg(Some(c)),
        );
    }
    {
        let interactive = interactive.clone();
        bind(move || if interactive() { 0 } else { text_attributes::DIM }, move |a| el.set_attributes(Some(a)));
    }
    if no_wrap {
        el.set_wrap_mode(WrapMode::None);
    }
    el.set_flex_shrink(Some(0.0));
    insert(el, View::from(format!("{key} ")), At::End);
    let s = span();
    insert_node(el, s, None);
    bind_always(
        move || if interactive() { Some(rgba(theme.with(|th| th.text.muted))) } else { None },
        move |c| s.set_span_style(c, None, 0, 0),
    );
    insert(s, View::from(label), At::Only);
    View::Node(el)
}

/// `createFrames({ style: "blocks" })` (ui/spinner.ts): the knight-rider scanner, 8 cells, a trail of 6.
fn spinner_frames() -> Vec<String> {
    let width = 8i64;
    let (hold_start, hold_end, trail) = (30i64, 9i64, 6i64);
    let total = width + hold_end + (width - 1) + hold_start;
    (0..total)
        .map(|frame| {
            let (active, holding, progress, forward) = if frame < width {
                (frame, false, 0, true)
            } else if frame < width + hold_end {
                (width - 1, true, frame - width, true)
            } else if frame < width + hold_end + width - 1 {
                (width - 2 - (frame - width - hold_end), false, 0, false)
            } else {
                (0, true, frame - width - hold_end - (width - 1), false)
            };
            (0..width)
                .map(|cell| {
                    let distance = if forward { active - cell } else { cell - active };
                    let index = if holding {
                        distance + progress
                    } else if distance > 0 && distance < trail {
                        distance
                    } else if distance == 0 {
                        0
                    } else {
                        -1
                    };
                    if index >= 0 && index < trail { "■" } else { "⬝" }
                })
                .collect::<String>()
        })
        .collect()
}

/// The running session's `prompt.footer.status`: the spinner and `PromptInterruptStatus`.
pub fn running_status(interrupt: Signal<u32>, animations: bool, agent_color: Memo<Option<RGBA>>) -> View {
    let theme = tui().theme;
    let row = box_();
    row.set_flex_direction(FlexDirection::Row);
    row.set_gap(2, Some(Value::Point(1.0)));
    row.set_flex_grow(Some(1.0));
    row.set_justify_content(Justify::FlexStart);
    let spinner = box_();
    insert_node(row, spinner, None);
    spinner.set_margin(Edge::Left, Some(Value::Point(1.0)));
    let indicator = text();
    insert_node(spinner, indicator, None);
    if !animations {
        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| indicator.set_fg(Some(c)));
        insert(indicator, View::from("[⋯]"), At::Only);
    } else {
        let frames = Rc::new(spinner_frames());
        let frame = create_signal(0usize).0;
        let stopped = Rc::new(Cell::new(false));
        fn tick(frame: Signal<usize>, count: usize, stopped: Rc<Cell<bool>>) {
            renderer::set_timeout(40, move || {
                if stopped.get() || !frame.is_alive() {
                    return;
                }
                frame.set((frame.get_untracked() + 1) % count);
                renderer::request_render();
                tick(frame, count, stopped);
            });
        }
        tick(frame, frames.len(), stopped.clone());
        on_cleanup(move || stopped.set(true));
        bind_always(
            move || agent_color.get().unwrap_or_else(|| rgba(theme.with(|th| th.border.base))),
            move |c| indicator.set_fg(Some(c)),
        );
        insert(indicator, View::dynamic(move || View::from(frames[frame.get()].clone())), At::Only);
    }
    // <PromptInterruptStatus armed animations text subdued warning flash>
    let status = text();
    insert_node(row, status, None);
    let armed = move || interrupt.get() > 0;
    bind_always(
        move || if armed() { rgba(theme.with(|th| th.text.feedback.warning.base)) } else { rgba(theme.with(|th| th.text.base)) },
        move |c| status.set_fg(Some(c)),
    );
    status.set_wrap_mode(WrapMode::None);
    status.set_truncate(true);
    status.set_flex_shrink(Some(1.0));
    insert(status, View::from("esc "), At::End);
    let s = span();
    insert_node(status, s, None);
    bind_always(
        move || if armed() { rgba(theme.with(|th| th.text.feedback.warning.base)) } else { rgba(theme.with(|th| th.text.muted)) },
        move |c| s.set_span_style(Some(c), None, 0, 0),
    );
    insert(s, View::dynamic(move || View::from(if armed() { "again to interrupt" } else { "interrupt" })), At::Only);
    // TODO(port-tui): the armed flash (an ignition tween from the warning's decreased tint) with animations on.
    View::Node(row)
}

/// The `/btw` plugin's command (`session.aside`), registered from the `app` slot.
pub fn install_btw() {
    let t = tui();
    let toast = t.toast.clone();
    let route = t.route;
    create_layer(move || {
        let toast = toast.clone();
        Layer {
            mode: Some("global"),
            commands: vec![
                Command::with_input("session.aside", move |input| {
                    if !route.with_untracked(|r| matches!(r, Route::Session { .. })) {
                        toast.show(ToastInput::new(Variant::Warning, "Open a session first"));
                        return true;
                    }
                    // TODO(port-tui): the dialogs slice: the question prompt and the answer dialog
                    // (`session.generate`).
                    let _ = input;
                    true
                })
                .title("Ask a side question")
                .description("One-shot answer from the session's context without adding to the conversation")
                .group("Session")
                .palette(true)
                .slash_arguments("btw"),
            ],
            ..Layer::default()
        }
    });
}
