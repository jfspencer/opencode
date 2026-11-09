//! The transcript's message views from `routes/session/index.tsx`: `SessionMessageView` (user, shell, switches,
//! notices, skills, compactions, execution errors), `AssistantFooter`, `AssistantRetry`, `FreeLimitNotice`,
//! `CompactionQueued`, `RevertMessage` and `QueuedPromptDock`.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, markdown, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Align, Edge, FlexDirection, Justify, MouseEventType};
use opencode_tui_markup::markdown::{InternalBlockMode, MarkdownTableOptions, MarkdownTableStyle};
use opencode_tui_sync::json::JsonExt;

use super::message_parts::{InlineToolRowProps, duration, inline_tool_row};
use super::render_context::{MarkdownMode, use_ctx};
use crate::border::split_border_chars;
use crate::ctx::tui;
use crate::spinner::{SPINNER_FRAMES, SpinnerElement, spinner_element};
use crate::syntax::current_syntax;
use crate::util::{rgba, string_width, titlecase, truncate_width};

pub type Message = Rc<dyn Fn() -> Rc<Json>>;

fn str_of(value: &Json, key: &str) -> String {
    value.str(key).unwrap_or_default().to_string()
}

/// `errorMessage(error)` for the structured errors the transcript shows.
pub fn error_message(error: Option<&Json>) -> String {
    let Some(error) = error else { return "unknown error".into() };
    if let Some(message) = error.str("message").filter(|m| !m.is_empty()) {
        return message.to_string();
    }
    if let Some(message) = error.get("data").and_then(|d| d.str("message")).filter(|m| !m.is_empty()) {
        return message.to_string();
    }
    match error {
        Json::String(s) if !s.is_empty() => s.clone(),
        Json::Object(_) => opencode_tui_sync::client::stringify(error),
        _ => "unknown error".into(),
    }
}

/// `local.agent.color(id)`: the visible agent's color, else the theme's categorical color of its index.
pub fn agent_color(id: &str) -> RGBA {
    crate::local::local().color(id)
}

fn padded(left: f32) -> opencode_tui_core::tree::Element {
    let el = box_();
    el.set_padding(Edge::Left, Some(Value::Point(left)));
    el
}

fn muted_text(content: impl Fn() -> String + 'static) -> opencode_tui_core::tree::Element {
    let theme = tui().theme;
    let el = text();
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| el.set_fg(Some(c)));
    insert(el, View::dynamic(move || View::from(content())), At::Only);
    el
}

/// `<SessionMessageView message>`.
pub fn session_message_view(message: Message) -> View {
    let kind = {
        let message = message.clone();
        create_memo(move || str_of(&message(), "type"))
    };
    let m = message.clone();
    switch(
        vec![
            when(move || kind.get() == "user", {
                let m = m.clone();
                move || user_message(m.clone())
            }),
            when(move || kind.get() == "shell", {
                let m = m.clone();
                move || super::parts::shell_message(m.clone())
            }),
            when(move || matches!(kind.get().as_str(), "agent-switched" | "model-switched" | "location-switched"), {
                let m = m.clone();
                move || switch_message(m.clone())
            }),
            when(move || matches!(kind.get().as_str(), "system" | "synthetic" | "skill"), {
                let m = m.clone();
                move || {
                    let m2 = m.clone();
                    let m3 = m.clone();
                    show(move || kind.get() == "skill", move || skill_message(m2.clone()), Some(Rc::new(move || notice_message(m3.clone()))))
                }
            }),
            when(move || kind.get() == "compaction", {
                let m = m.clone();
                move || compaction_message(m.clone())
            }),
            when(
                {
                    let m = m.clone();
                    move || kind.get() == "idle" && m().get("error").is_some()
                },
                {
                    let m = m.clone();
                    move || execution_error(m.clone())
                },
            ),
        ],
        None,
    )
}

/// `<ExecutionError error>`.
fn execution_error(message: Message) -> View {
    let theme = tui().theme;
    let wrap = padded(3.0);
    let label = text();
    insert_node(wrap, label, None);
    bind_always(move || rgba(theme.with(|th| th.text.feedback.error.base)), move |c| label.set_fg(Some(c)));
    insert(label, View::dynamic(move || View::from(format!("Error: {}", error_message(message().get("error"))))), At::Only);
    View::Node(wrap)
}

/// `<UserMessage message>`.
pub fn user_message(message: Message) -> View {
    let ctx = use_ctx();
    let t = tui();
    let theme = t.theme;
    let data = t.data.clone();
    let (hover, set_hover) = create_signal(false);
    let session_id = ctx.session_id.clone();
    let color = create_memo_equals(
        move || {
            let agent = data.store.session.get(&session_id).and_then(|s| s.str("agent").map(str::to_string)).unwrap_or_else(|| "build".into());
            agent_color(&agent)
        },
        None,
    );
    let delivery = {
        let (ctx, message) = (ctx.clone(), message.clone());
        create_memo(move || (ctx.pending_delivery)(&str_of(&message(), "id")))
    };
    let files = {
        let message = message.clone();
        create_memo(move || message().arr("files").cloned().unwrap_or_default())
    };
    let skills = {
        let message = message.clone();
        create_memo(move || message().arr("skills").cloned().unwrap_or_default())
    };
    let m = message.clone();
    show(
        move || !str_of(&m(), "text").trim().is_empty() || !files.with(Vec::is_empty) || !skills.with(Vec::is_empty),
        move || {
            let outer = box_();
            outer.set_border(BorderProp::Sides(vec![Side::Left]));
            bind_always(
                move || if delivery.get().is_some() { rgba(theme.with(|th| th.border.base)) } else { color.get() },
                move |c| outer.set_border_color(Some(c)),
            );
            outer.set_custom_border_chars(Some(split_border_chars()));
            bind_always(move || rgba(theme.with(|th| th.background.raised.base)), move |c| outer.set_background_color(Some(c)));
            // TODO(port-tui): `<SessionImages>` (image previews) with the media sources.
            let inner = box_();
            insert_node(outer, inner, None);
            inner.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_hover.set(true))));
            inner.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_hover.set(false))));
            // TODO(port-tui): the dialogs slice: the pending-steer DialogSelect and DialogMessage on mouse up.
            inner.set_padding(Edge::Top, Some(Value::Point(1.0)));
            inner.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
            inner.set_padding(Edge::Left, Some(Value::Point(2.0)));
            inner.set_padding(Edge::Right, Some(Value::Point(2.0)));
            bind_always(
                move || {
                    rgba(theme.with(|th| if hover.get() { th.decrease(th.background.raised.base, 1.0) } else { th.background.raised.base }))
                },
                move |c| inner.set_background_color(Some(c)),
            );
            inner.set_flex_shrink(Some(0.0));
            let body = text();
            insert_node(inner, body, None);
            bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| body.set_fg(Some(c)));
            let m = message.clone();
            insert(body, View::dynamic(move || View::from(str_of(&m(), "text"))), At::Only);
            insert(inner, chips(skills, true), At::End);
            insert(inner, chips(files, false), At::End);
            View::Node(outer)
        },
        None,
    )
}

/// The skill and file chips under a user message.
fn chips(items: Memo<Vec<Json>>, skill: bool) -> View {
    let t = tui();
    let theme = t.theme;
    show(
        move || !items.with(Vec::is_empty),
        move || {
            let row = box_();
            row.set_flex_direction(FlexDirection::Row);
            row.set_padding(Edge::Top, Some(Value::Point(1.0)));
            row.set_gap(2, Some(Value::Point(1.0)));
            row.set_flex_wrap(1);
            insert(
                row,
                for_each(
                    move || items.get(),
                    |item| opencode_tui_sync::client::stringify(item),
                    move |item, _| {
                        let chip = text();
                        bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| chip.set_fg(Some(c)));
                        let label = span();
                        insert_node(chip, label, None);
                        bind_always(
                            move || {
                                theme.with(|th| {
                                    let light = tui().theme.with(|_| false);
                                    let _ = light;
                                    (rgba(th.hue.accent[200]), rgba(th.background.raised.base))
                                })
                            },
                            move |(bg, fg)| label.set_span_style(Some(fg), Some(bg), text_attributes::BOLD, text_attributes::BOLD),
                        );
                        let name = span();
                        insert_node(chip, name, None);
                        bind_always(
                            move || theme.with(|th| (rgba(th.decrease(th.background.raised.base, 1.0)), rgba(th.text.muted))),
                            move |(bg, fg)| name.set_span_style(Some(fg), Some(bg), 0, 0),
                        );
                        if skill {
                            insert_node(label, text_node_from_string(" skill "), None);
                            insert(name, View::dynamic(move || View::from(format!(" {} ", str_of(&item.get(), "name")))), At::Only);
                        } else {
                            insert(
                                label,
                                View::dynamic(move || {
                                    let dir = item.get().str("mime") == Some("application/x-directory");
                                    View::from(format!(" {} ", if dir { "dir" } else { "file" }))
                                }),
                                At::Only,
                            );
                            insert(
                                name,
                                View::dynamic(move || {
                                    let file = item.get();
                                    let name = file.str("name").map(str::to_string).unwrap_or_else(|| {
                                        let source = file.get("source");
                                        if source.and_then(|s| s.str("type")) == Some("uri") {
                                            source.and_then(|s| s.str("uri")).unwrap_or_default().to_string()
                                        } else {
                                            "attachment".to_string()
                                        }
                                    });
                                    View::from(format!(" {name} "))
                                }),
                                At::Only,
                            );
                        }
                        View::Node(chip)
                    },
                    None,
                ),
                At::Only,
            );
            View::Node(row)
        },
        None,
    )
}

fn model_name(models: &[Rc<Json>], provider: &str, id: &str) -> Option<String> {
    models
        .iter()
        .find(|m| m.str("providerID") == Some(provider) && m.str("id") == Some(id))
        .and_then(|m| m.str("name").map(str::to_string))
}

/// `<AssistantFooter message>`.
pub fn assistant_footer(message: Message) -> View {
    let ctx = use_ctx();
    let t = tui();
    let theme = t.theme;
    let config = t.config_context.clone();
    let data = t.data.clone();
    let model = {
        let (ctx, message) = (ctx.clone(), message.clone());
        create_memo(move || {
            let m = message();
            let model = m.get("model");
            let provider = model.and_then(|m| m.str("providerID")).unwrap_or_default().to_string();
            let id = model.and_then(|m| m.str("id")).unwrap_or_default().to_string();
            model_name(&(ctx.models)(), &provider, &id).unwrap_or(format!("{provider}/{id}"))
        })
    };
    let session_id = ctx.session_id.clone();
    let turn = {
        let (data, session_id, message, ctx) = (data.clone(), session_id.clone(), message.clone(), ctx.clone());
        create_memo_equals(
            move || {
                let m = message();
                let messages = data.store.session.message.list(&session_id);
                let index = (ctx.message_index)(&str_of(&m, "id"));
                let legacy = (ctx.legacy_turns)();
                (turn_duration(&messages, &m, index, legacy), tokens_per_second(&messages, &m, index, legacy))
            },
            None,
        )
    };
    let interrupted = {
        let message = message.clone();
        create_memo(move || message().get("error").and_then(|e| e.str("message")) == Some("Step interrupted"))
    };
    let free_limit = {
        let message = message.clone();
        create_memo(move || message().get("error").and_then(|e| e.str("type")) == Some("provider.free-limit"))
    };
    let has_error = {
        let message = message.clone();
        create_memo(move || message().get("error").is_some())
    };
    let has_retry = {
        let message = message.clone();
        create_memo(move || message().get("retry").is_some())
    };
    let m = message.clone();
    let error_view = show(
        move || has_error.get() && !interrupted.get() && !has_retry.get(),
        move || {
            let m = m.clone();
            let m2 = m.clone();
            switch(
                vec![
                    when(move || free_limit.get(), move || free_limit_notice(m.clone())),
                    // TODO(port-tui): `useLimitNotice` / `<UsageLimitNotice>` (component/usage.tsx) for go limits.
                ],
                Some(Rc::new(move || {
                    let wrap = padded(3.0);
                    let label = text();
                    insert_node(wrap, label, None);
                    bind_always(move || rgba(theme.with(|th| th.text.feedback.error.base)), move |c| label.set_fg(Some(c)));
                    let m = m2.clone();
                    insert(label, View::dynamic(move || View::from(format!("Error: {}", error_message(m().get("error"))))), At::Only);
                    View::Node(wrap)
                })),
            )
        },
        None,
    );
    let retry = {
        let message = message.clone();
        assistant_retry(Rc::new(move || message().get("retry").cloned()))
    };
    let line = padded(3.0);
    bind(move || if has_retry.get() || (has_error.get() && !interrupted.get()) { 1.0 } else { 0.0 }, move |m| {
        line.set_margin(Edge::Top, Some(Value::Point(m)))
    });
    let label = text();
    insert_node(line, label, None);
    let agent = span();
    insert_node(label, agent, None);
    {
        let msg = message.clone();
        bind_always(
            move || {
                let m = msg();
                if m.get("error").is_some() { rgba(theme.with(|th| th.text.muted)) } else { agent_color(&str_of(&m, "agent")) }
            },
            move |c| agent.set_span_style(Some(c), None, 0, 0),
        );
        let message = message.clone();
        insert(agent, View::dynamic(move || View::from(titlecase(&str_of(&message(), "agent")))), At::Only);
    }
    let muted_span = move |content: Rc<dyn Fn() -> String>| {
        let s = span();
        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| s.set_span_style(Some(c), None, 0, 0));
        insert(s, View::dynamic(move || View::from(content())), At::Only);
        View::Node(s)
    };
    let tw = ctx.terminal_width.clone();
    insert(label, show(move || tw() >= 28, move || muted_span(Rc::new(move || format!(" · {}", model.get()))), None), At::End);
    let tw = ctx.terminal_width.clone();
    insert(
        label,
        show(
            move || turn.get().0 > 0.0 && (tw() < 28 || tw() >= 36),
            move || muted_span(Rc::new(move || format!(" · {}", duration(turn.get().0)))),
            None,
        ),
        At::End,
    );
    insert(
        label,
        show_value(
            move || if config.data().session.tps { turn.get().1.map(|v| (v * 10.0).round() as i64) } else { None },
            move |_| muted_span(Rc::new(move || format!(" · {:.1} tok/s", turn.get().1.unwrap_or(0.0)))),
            None,
        ),
        At::End,
    );
    insert(label, show(move || interrupted.get(), move || muted_span(Rc::new(|| " · interrupted".to_string())), None), At::End);
    View::list([error_view, retry, View::Node(line)])
}

fn input_index(messages: &[Rc<Json>], end: usize, legacy: bool) -> Option<usize> {
    let mut input = None;
    for index in (0..end).rev() {
        let kind = messages[index].str("type");
        if kind == Some("idle") {
            return input;
        }
        if kind != Some("user") && kind != Some("synthetic") {
            continue;
        }
        if legacy {
            return Some(index);
        }
        input = Some(index);
    }
    input
}

/// `SessionTurn.duration(messages, message, { index, legacy })`.
pub fn turn_duration(messages: &[Rc<Json>], message: &Json, index: Option<usize>, legacy: bool) -> f64 {
    let Some(completed) = message.get("time").and_then(|t| t.num("completed")) else { return 0.0 };
    let index = index.unwrap_or(messages.len());
    let input = input_index(messages, index, legacy).and_then(|i| messages.get(i));
    let start = input.and_then(|m| m.get("time").and_then(|t| t.num("created"))).or_else(|| message.get("time").and_then(|t| t.num("created"))).unwrap_or(0.0);
    (completed - start).max(0.0)
}

/// `SessionTurn.tokensPerSecond(messages, message, { index, legacy })`.
pub fn tokens_per_second(messages: &[Rc<Json>], message: &Json, index: Option<usize>, legacy: bool) -> Option<f64> {
    let _ = message;
    let end = index.map_or(messages.len(), |i| i + 1);
    let start = input_index(messages, end, legacy).map_or(0, |i| i + 1);
    let steps: Vec<&Rc<Json>> = messages[start.min(end)..end].iter().filter(|m| m.str("type") == Some("assistant")).collect();
    let durations: Vec<f64> = steps
        .iter()
        .filter_map(|s| {
            let time = s.get("time")?;
            Some((time.num("streamed")? - time.num("created").unwrap_or(0.0)).max(0.0))
        })
        .collect();
    if steps.is_empty() || durations.len() != steps.len() {
        return None;
    }
    let output: f64 = steps
        .iter()
        .map(|s| s.get("tokens").map_or(0.0, |t| t.num("output").unwrap_or(0.0) + t.num("reasoning").unwrap_or(0.0)))
        .sum();
    let total: f64 = durations.iter().sum();
    if output <= 0.0 || total <= 0.0 {
        return None;
    }
    Some(output / (total / 1000.0))
}

/// `<AssistantRetry retry>`.
fn assistant_retry(retry: Rc<dyn Fn() -> Option<Json>>) -> View {
    let theme = tui().theme;
    let (seconds, set_seconds) = create_signal(0i64);
    {
        let retry = retry.clone();
        create_effect(move || {
            let Some(at) = retry().and_then(|r| r.num("at")) else { return };
            let update = move || {
                let now = opencode_tui_sync::runtime::now();
                let value = ((at - now) / 1000.0).ceil().max(0.0) as i64;
                set_seconds.set(value);
                value
            };
            if update() == 0 {
                return;
            }
            let alive = Rc::new(std::cell::Cell::new(true));
            fn schedule(alive: Rc<std::cell::Cell<bool>>, update: Rc<dyn Fn() -> i64>) {
                renderer::set_timeout(1000, move || {
                    if !alive.get() {
                        return;
                    }
                    if update() != 0 {
                        schedule(alive, update);
                    }
                });
            }
            schedule(alive.clone(), Rc::new(update));
            on_cleanup(move || alive.set(false));
        });
    }
    show_value(
        move || retry(),
        move |retry| {
            let wrap = padded(3.0);
            let label = text();
            insert_node(wrap, label, None);
            bind_always(move || rgba(theme.with(|th| th.text.feedback.warning.base)), move |c| label.set_fg(Some(c)));
            insert(
                label,
                View::dynamic(move || {
                    let r = retry.get().unwrap_or(Json::Null);
                    let s = seconds.get();
                    let due = if s > 0 { format!("Retrying in {s}s") } else { "Retry due".to_string() };
                    let attempt = r.num("attempt").unwrap_or(0.0);
                    View::from(format!("⚠ {due} · attempt {attempt} · {}", r.get("error").and_then(|e| e.str("message")).unwrap_or_default()))
                }),
                At::Only,
            );
            View::Node(wrap)
        },
        None,
    )
}

/// `<FreeLimitNotice message>`.
fn free_limit_notice(message: Message) -> View {
    let theme = tui().theme;
    let column = padded(3.0);
    column.set_flex_direction(FlexDirection::Column);
    // TODO(port-tui): `freeLimitReset` with the locale's time and date formats.
    let _ = message;
    insert_node(column, muted_text(|| "You've used today's free messages.".to_string()), None);
    let second = text();
    insert_node(column, second, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| second.set_fg(Some(c)));
    insert_node(second, text_node_from_string("Upgrade to OpenCode Go → "), None);
    let connect = span();
    insert_node(second, connect, None);
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| connect.set_span_style(Some(c), None, 0, 0));
    insert_node(connect, text_node_from_string("/connect"), None);
    insert_node(second, text_node_from_string(" · Switch model → "), None);
    let models = span();
    insert_node(second, models, None);
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| models.set_span_style(Some(c), None, 0, 0));
    insert_node(models, text_node_from_string("/models"), None);
    View::Node(column)
}

/// `<SessionSwitchMessageV2 message>`.
fn switch_message(message: Message) -> View {
    let ctx = use_ctx();
    let theme = tui().theme;
    let m = message();
    if m.str("type") == Some("location-switched") {
        let wrap = padded(3.0);
        let label = text();
        insert_node(wrap, label, None);
        let prefix = span();
        insert_node(label, prefix, None);
        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| prefix.set_span_style(Some(c), None, 0, 0));
        insert_node(prefix, text_node_from_string("↳ Moved to "), None);
        let place = span();
        insert_node(label, place, None);
        bind_always(move || rgba(theme.with(|th| th.text.feedback.info.base)), move |c| place.set_span_style(Some(c), None, 0, 0));
        insert(place, View::dynamic(move || View::from(message().get("location").and_then(|l| l.str("directory")).unwrap_or_default().to_string())), At::Only);
        return View::Node(wrap);
    }
    let wrap = padded(3.0);
    insert_node(
        wrap,
        muted_text(move || {
            let m = message();
            match m.str("type") {
                Some("agent-switched") => {
                    let agent = titlecase(&str_of(&m, "agent"));
                    match m.str("previous").filter(|p| Some(*p) != m.str("agent")) {
                        Some(previous) => format!("Switched agent from {} to {agent}", titlecase(previous)),
                        None => format!("Switched agent to {agent}"),
                    }
                }
                Some("model-switched") => switch_label(m.get("model"), &(ctx.models)(), m.get("previous")),
                _ => String::new(),
            }
        }),
        None,
    );
    View::Node(wrap)
}

/// `switchLabel(model, models, previous)` (util/model.ts).
fn switch_label(model: Option<&Json>, models: &[Rc<Json>], previous: Option<&Json>) -> String {
    let provider = model.and_then(|m| m.str("providerID")).unwrap_or_default();
    let id = model.and_then(|m| m.str("id")).unwrap_or_default();
    let variant = model.and_then(|m| m.str("variant"));
    if let Some(previous) = previous {
        if previous.str("providerID") == Some(provider) && previous.str("id") == Some(id) {
            return format!("Switched variant to {}", variant.unwrap_or("default"));
        }
    }
    match model_name(models, provider, id) {
        None => {
            let parts: Vec<&str> = [Some(provider), Some(id), variant].into_iter().flatten().collect();
            format!("Switched model to {}", parts.join("/"))
        }
        Some(display) => {
            let variant = variant.filter(|v| *v != "default").map(|v| format!(" ({v})")).unwrap_or_default();
            format!("Switched model to {display}{variant}")
        }
    }
}

/// `<SessionNoticeMessageV2 message>`.
fn notice_message(message: Message) -> View {
    let ctx = use_ctx();
    let t = tui();
    let theme = t.theme;
    let route = t.route;
    let (hover, set_hover) = create_signal(false);
    let metadata = {
        let message = message.clone();
        move || {
            let m = message();
            if m.str("type") == Some("synthetic") { m.get("metadata").cloned() } else { None }
        }
    };
    let field = {
        let metadata = metadata.clone();
        Rc::new(move |key: &str| metadata().and_then(|m| m.str(key).map(str::to_string)))
    };
    let source = {
        let field = field.clone();
        move || field("source")
    };
    let completion = {
        let source = source.clone();
        create_memo(move || matches!(source().as_deref(), Some("subagent" | "shell")))
    };
    let child_id = {
        let (field, source) = (field.clone(), source.clone());
        Rc::new(move || if source().as_deref() == Some("subagent") { field("childID") } else { None })
    };
    let state = {
        let field = field.clone();
        Rc::new(move || field("state"))
    };
    let actor = {
        let (field, source) = (field.clone(), source.clone());
        move || if source().as_deref() == Some("shell") { "Shell".to_string() } else { titlecase(&field("agent").unwrap_or_else(|| "Subagent".into())) }
    };
    let text_of = {
        let message = message.clone();
        Rc::new(move || {
            let m = message();
            match m.str("type") {
                Some("system") => m.str("description").unwrap_or("Instructions updated").to_string(),
                Some("synthetic") => m.str("description").unwrap_or_default().to_string(),
                _ => String::new(),
            }
        })
    };
    let source_for = source.clone();
    let fallback_text = text_of.clone();
    show(
        move || completion.get(),
        {
            let (state, child_id) = (state.clone(), child_id.clone());
            move || {
                let description = {
                    let (text_of, source) = (text_of.clone(), source.clone());
                    move || {
                        let text = text_of();
                        if source().as_deref() == Some("shell") { text.split_whitespace().collect::<Vec<_>>().join(" ") } else { text }
                    }
                };
                let status = {
                    let state = state.clone();
                    move || match state().as_deref() {
                        Some("completed") => "finished".to_string(),
                        Some("error") => "failed".to_string(),
                        Some(other) => other.to_string(),
                        None => "finished".to_string(),
                    }
                };
                let heading = {
                    let (state, actor, status) = (state.clone(), actor.clone(), status.clone());
                    Rc::new(move || format!("{} {} {}", if state().as_deref() == Some("completed") { "↳" } else { "!" }, actor(), status()))
                };
                let wrap = box_();
                wrap.set_margin(Edge::Left, Some(Value::Point(3.0)));
                let c1 = child_id.clone();
                wrap.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| {
                    if c1().is_some() {
                        set_hover.set(true)
                    }
                })));
                wrap.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_hover.set(false))));
                let c2 = child_id.clone();
                wrap.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| {
                    if let Some(id) = c2() {
                        route.set(crate::ctx::Route::Session { session_id: id });
                    }
                })));
                let label = text();
                insert_node(wrap, label, None);
                label.set_wrap_mode(WrapMode::None);
                let head = span();
                insert_node(label, head, None);
                let (state, child_id) = (state.clone(), child_id.clone());
                bind_always(
                    move || {
                        theme.with(|th| match state().as_deref() {
                            Some("error") => rgba(th.text.feedback.error.base),
                            Some("cancelled") => rgba(th.text.feedback.warning.base),
                            _ if hover.get() && child_id().is_some() => rgba(th.text.base),
                            _ => rgba(th.text.feedback.info.base),
                        })
                    },
                    move |c| head.set_span_style(Some(c), None, 0, 0),
                );
                let h = heading.clone();
                insert(head, View::dynamic(move || View::from(h())), At::Only);
                let tail = span();
                insert_node(label, tail, None);
                bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| tail.set_span_style(Some(c), None, 0, 0));
                let (ctx, heading) = (ctx.clone(), heading.clone());
                insert(
                    tail,
                    View::dynamic(move || {
                        let width = ((ctx.width)() as i64 - 3 - string_width(&heading()) as i64).max(0);
                        View::from(truncate_width(&format!(" · {}", description()), width))
                    }),
                    At::Only,
                );
                View::Node(wrap)
            }
        },
        Some(Rc::new(move || {
            let source = source_for.clone();
            let text_of = fallback_text.clone();
            inline_tool_row(InlineToolRowProps {
                icon: Rc::new(move || if source().as_deref() == Some("goal") { "◎".into() } else { "◈".into() }),
                color: Some(Rc::new(move || Some(rgba(theme.with(|th| th.text.muted))))),
                pending: Rc::new(|| "Notice".into()),
                complete: Rc::new(|| true),
                children: Rc::new(move || View::from(text_of())),
                ..InlineToolRowProps::default()
            })
        })),
    )
}

/// `<SessionSkillMessage message>`.
fn skill_message(message: Message) -> View {
    let theme = tui().theme;
    inline_tool_row(InlineToolRowProps {
        icon: Rc::new(|| "→".into()),
        color: Some(Rc::new(move || Some(rgba(theme.with(|th| th.text.muted))))),
        pending: Rc::new(|| "Skill".into()),
        complete: Rc::new(|| true),
        children: Rc::new(move || View::from(format!("Skill {}", str_of(&message(), "name")))),
        ..InlineToolRowProps::default()
    })
}

fn rule(color: Rc<dyn Fn() -> RGBA>) -> opencode_tui_core::tree::Element {
    let line = box_();
    line.set_border(BorderProp::Sides(vec![Side::Top]));
    bind_always(move || color(), move |c| line.set_border_color(Some(c)));
    line.set_flex_grow(Some(1.0));
    line
}

/// `<CompactionMessage message>`.
fn compaction_message(message: Message) -> View {
    let ctx = use_ctx();
    let t = tui();
    let theme = t.theme;
    let status = {
        let message = message.clone();
        create_memo(move || str_of(&message(), "status"))
    };
    let cancelled = {
        let message = message.clone();
        create_memo(move || {
            let m = message();
            m.str("status") == Some("failed") && m.get("error").and_then(|e| e.str("type")) == Some("aborted")
        })
    };
    let content = {
        let message = message.clone();
        create_memo(move || {
            let m = message();
            let text = if m.str("status") == Some("failed") {
                if cancelled.get() { String::new() } else { error_message(m.get("error")) }
            } else {
                str_of(&m, "summary")
            };
            text.trim().to_string()
        })
    };
    let color: Rc<dyn Fn() -> RGBA> = Rc::new(move || {
        theme.with(|th| if status.get() == "failed" && !cancelled.get() { rgba(th.text.feedback.error.base) } else { rgba(th.text.muted) })
    });
    let usage = {
        let message = message.clone();
        create_memo(move || {
            let m = message();
            if m.str("status") == Some("running") {
                return None;
            }
            let tokens = m.get("tokens")?;
            let n = |k: &str| tokens.num(k).unwrap_or(0.0);
            let cache = |k: &str| tokens.get("cache").and_then(|c| c.num(k)).unwrap_or(0.0);
            let input = n("input") + cache("read") + cache("write");
            let output = n("output") + n("reasoning");
            if input + output <= 0.0 {
                return None;
            }
            Some(format!("{} in · {} out", locale_number(input), locale_number(output)))
        })
    };
    let outer = box_();
    let row = box_();
    insert_node(outer, row, None);
    row.set_flex_direction(FlexDirection::Row);
    row.set_align_items(Align::Center);
    insert_node(row, rule(color.clone()), None);
    let middle = box_();
    insert_node(row, middle, None);
    middle.set_flex_direction(FlexDirection::Row);
    middle.set_gap(2, Some(Value::Point(1.0)));
    middle.set_padding(Edge::Left, Some(Value::Point(1.0)));
    middle.set_padding(Edge::Right, Some(Value::Point(1.0)));
    let config = ctx.config.clone();
    let c = color.clone();
    insert(
        middle,
        switch(
            vec![
                when(move || status.get() == "running", {
                    let c = c.clone();
                    let config = config.clone();
                    move || {
                        let c = c.clone();
                        let c2 = c.clone();
                        let config = config.clone();
                        show(
                            move || config().animations.unwrap_or(true),
                            move || {
                                let el = spinner_element(c.clone());
                                el.set_spinner_frames(SPINNER_FRAMES.iter().map(|f| f.to_string()).collect());
                                View::Node(el)
                            },
                            Some(Rc::new(move || {
                                let el = text();
                                let c = c2.clone();
                                bind_always(move || c(), move |c| el.set_fg(Some(c)));
                                insert(el, View::from("⋯"), At::Only);
                                View::Node(el)
                            })),
                        )
                    }
                }),
                when(move || status.get() == "failed" && !cancelled.get(), {
                    let c = c.clone();
                    move || {
                        let el = text();
                        let c = c.clone();
                        bind_always(move || c(), move |c| el.set_fg(Some(c)));
                        insert(el, View::from("✗"), At::Only);
                        View::Node(el)
                    }
                }),
            ],
            None,
        ),
        At::End,
    );
    let colored_text = |content: Rc<dyn Fn() -> String>, color: Rc<dyn Fn() -> RGBA>| {
        let el = text();
        bind_always(move || color(), move |c| el.set_fg(Some(c)));
        insert(el, View::dynamic(move || View::from(content())), At::Only);
        el
    };
    {
        let message = message.clone();
        let title = colored_text(
            Rc::new(move || {
                let m = message();
                if m.str("status") == Some("completed") && m.get("native").and_then(|n| n.get("messages")).is_some() {
                    "Provider compaction".to_string()
                } else {
                    "Compaction".to_string()
                }
            }),
            color.clone(),
        );
        insert_node(middle, title, None);
    }
    let c = color.clone();
    insert(middle, show(move || cancelled.get(), move || View::Node(colored_text(Rc::new(|| "· cancelled".into()), c.clone())), None), At::End);
    let c = color.clone();
    insert(
        middle,
        show(move || usage.get().is_some(), move || View::Node(colored_text(Rc::new(move || format!("· {}", usage.get().unwrap_or_default())), c.clone())), None),
        At::End,
    );
    insert_node(row, rule(color.clone()), None);
    let markdown_mode = ctx.markdown_mode.clone();
    insert(
        outer,
        show(
            move || !content.get().is_empty(),
            move || {
                let wrap = box_();
                wrap.set_padding(Edge::Top, Some(Value::Point(1.0)));
                wrap.set_padding(Edge::Left, Some(Value::Point(3.0)));
                let md = markdown();
                insert_node(wrap, md, None);
                bind_always(current_syntax, move |s| md.set_markdown_syntax_style(s));
                md.set_markdown_streaming(true);
                md.set_markdown_internal_block_mode(InternalBlockMode::TopLevel);
                bind(move || content.get(), move |c| md.set_markdown_content(&c));
                md.set_markdown_table_options(Some(MarkdownTableOptions {
                    style: Some(MarkdownTableStyle::Grid),
                    cell_padding_x: Some(1.0),
                    ..MarkdownTableOptions::default()
                }));
                let markdown_mode = markdown_mode.clone();
                bind(move || markdown_mode() == MarkdownMode::Rendered, move |c| md.set_markdown_conceal(c));
                bind_always(move || rgba(theme.with(|th| th.markdown.text)), move |c| md.set_markdown_fg(Some(c)));
                bind_always(move || rgba(theme.with(|th| th.background.base)), move |c| md.set_markdown_bg(Some(c)));
                View::Node(wrap)
            },
            None,
        ),
        At::End,
    );
    View::Node(outer)
}

/// `Locale.number(num)`.
pub fn locale_number(num: f64) -> String {
    if num >= 999950.0 {
        return format!("{:.1}M", num / 1000000.0);
    }
    if num >= 1000.0 {
        return format!("{:.1}K", num / 1000.0);
    }
    format!("{num}")
}

/// `<CompactionQueued>`.
pub fn compaction_queued() -> View {
    let theme = tui().theme;
    let row = box_();
    row.set_flex_direction(FlexDirection::Row);
    row.set_align_items(Align::Center);
    let border: Rc<dyn Fn() -> RGBA> = Rc::new(move || rgba(theme.with(|th| th.border.base)));
    insert_node(row, rule(border.clone()), None);
    let middle = box_();
    insert_node(row, middle, None);
    middle.set_flex_direction(FlexDirection::Row);
    middle.set_gap(2, Some(Value::Point(1.0)));
    middle.set_padding(Edge::Left, Some(Value::Point(1.0)));
    middle.set_padding(Edge::Right, Some(Value::Point(1.0)));
    insert_node(middle, muted_text(|| "◇".into()), None);
    insert_node(middle, muted_text(|| "Compaction queued".into()), None);
    insert_node(row, rule(border), None);
    let _ = Justify::FlexStart;
    View::Node(row)
}
