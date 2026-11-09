//! The transcript's trailing rows from `routes/session/index.tsx`: `BackgroundToolHint`, `RevertMessage` and
//! `QueuedPromptDock`, and `FilePath` (ui/file-path.tsx).
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::{box_, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, MouseEventType};
use opencode_tui_core::color::RGBA;
use opencode_tui_sync::json::JsonExt;

use super::message_parts::canonical_tool_name;
use crate::border::split_border_chars;
use crate::ctx::tui;
use crate::util::{rgba, string_width, truncate_file_path};

const BACKGROUND_TOOL_HINT_DELAY: f64 = 3_000.0;

/// `<FilePath value maxWidth fg basenameFg>`.
pub fn file_path(value: Rc<dyn Fn() -> String>, max_width: Rc<dyn Fn() -> i64>, fg: Rc<dyn Fn() -> Option<RGBA>>) -> Element {
    let display = create_memo(move || {
        let value = truncate_file_path(&value(), max_width());
        let index = value.rfind(['/', '\\']).map_or(0, |i| i + 1);
        (value[..index].to_string(), value[index..].to_string())
    });
    let el = text();
    {
        let fg = fg.clone();
        bind_always(move || fg(), move |c| el.set_fg(c));
    }
    el.set_wrap_mode(WrapMode::None);
    el.set_truncate(true);
    let parent = span();
    insert_node(el, parent, None);
    {
        let fg = fg.clone();
        bind_always(move || fg(), move |c| parent.set_span_style(c, None, 0, 0));
    }
    insert(parent, View::dynamic(move || View::from(display.get().0)), At::Only);
    let base = span();
    insert_node(el, base, None);
    bind_always(move || fg(), move |c| base.set_span_style(c, None, 0, 0));
    insert(base, View::dynamic(move || View::from(display.get().1)), At::Only);
    el
}

/// `<BackgroundToolHint messages>`: after a shell or subagent tool has run for a while, the key that moves it to
/// the background.
pub fn background_tool_hint(messages: Rc<dyn Fn() -> Rc<Vec<Rc<Json>>>>) -> View {
    let theme = tui().theme;
    let shortcut = crate::keymap::use_shortcut("session.background");
    let running = {
        let shortcut = shortcut.clone();
        create_memo(move || {
            shortcut()?;
            let messages = messages();
            let current = messages
                .iter()
                .rev()
                .find(|m| m.str("type") == Some("assistant") && m.get("time").and_then(|t| t.num("completed")).is_none())?;
            let part = current.arr("content")?.iter().find(|p| {
                if p.str("type") != Some("tool") || p.get("state").and_then(|s| s.str("status")) != Some("running") {
                    return false;
                }
                let name = canonical_tool_name(p.str("name").unwrap_or_default());
                name == "shell" || name == "subagent"
            })?;
            let time = part.get("time");
            let started = time.and_then(|t| t.num("ran")).or_else(|| time.and_then(|t| t.num("created"))).unwrap_or(0.0);
            Some((format!("{}:{}", current.str("id").unwrap_or_default(), part.str("id").unwrap_or_default()), (started * 1000.0) as i64))
        })
    };
    // `createDelayedPresence(running, delay, equals)`.
    let (visible, set_visible) = create_signal(false);
    create_effect(move || {
        let current = running.get();
        set_visible.set(false);
        let Some((_, started)) = current else { return };
        let remaining = (BACKGROUND_TOOL_HINT_DELAY - (opencode_tui_sync::runtime::now() - started as f64 / 1000.0)).max(0.0);
        if remaining <= 0.0 {
            set_visible.set(true);
            return;
        }
        let timer = renderer::set_timeout(remaining as u64, move || set_visible.set(true));
        on_cleanup(move || renderer::clear_timeout(timer));
    });
    show_value(
        move || if visible.get() { shortcut() } else { None },
        move |value| {
            let wrap = box_();
            wrap.set_margin(Edge::Top, Some(Value::Point(1.0)));
            wrap.set_padding(Edge::Left, Some(Value::Point(3.0)));
            wrap.set_flex_shrink(Some(0.0));
            let label = text();
            insert_node(wrap, label, None);
            bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| label.set_fg(Some(c)));
            insert_node(label, text_node_from_string("Press "), None);
            let key = span();
            insert_node(label, key, None);
            bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| key.set_span_style(Some(c), None, 0, 0));
            insert(key, View::dynamic(move || View::from(value.get().unwrap_or_default())), At::Only);
            insert_node(label, text_node_from_string(" to move running work to the background"), None);
            View::Node(wrap)
        },
        None,
    )
}

/// `statusLabel(status)` (dialog-workspace-file-changes.tsx).
fn status_label(status: &str) -> &'static str {
    match status {
        "added" => "A",
        "deleted" => "D",
        _ => "M",
    }
}

/// `<RevertMessage count files>`.
pub fn revert_message(count: Rc<dyn Fn() -> usize>, files: Rc<dyn Fn() -> Vec<Json>>) -> View {
    let ctx = super::render_context::use_ctx();
    let t = tui();
    let theme = t.theme;
    let (hover, set_hover) = create_signal(false);
    let redo_key = crate::keymap::use_shortcut("session.redo");
    let outer = box_();
    outer.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_hover.set(true))));
    outer.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_hover.set(false))));
    {
        let (client, session_id, toast) = (t.data.client.clone(), ctx.session_id.clone(), t.toast.clone());
        outer.set_on_mouse(
            Some(MouseEventType::Up),
            Some(Rc::new(move |_| {
                // TODO(port-tui): the renderer's selection (`renderer.getSelection()?.getSelectedText()`).
                let (client, session_id, toast) = (client.clone(), session_id.clone(), toast.clone());
                tokio::task::spawn_local(async move {
                    let input = Json::Object([("sessionID".to_string(), Json::String(session_id))].into_iter().collect());
                    let result = client.call_named("session.revert.clear", &input, Default::default()).await;
                    if let Err(error) = result {
                        toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message()).duration(5000));
                    }
                });
            })),
        );
    }
    outer.set_flex_shrink(Some(0.0));
    outer.set_margin(Edge::Top, Some(Value::Point(1.0)));
    outer.set_border(BorderProp::Sides(vec![Side::Left]));
    outer.set_custom_border_chars(Some(split_border_chars()));
    bind_always(move || rgba(theme.with(|th| th.background.raised.base)), move |c| outer.set_border_color(Some(c)));
    let inner = box_();
    insert_node(outer, inner, None);
    for edge in [Edge::Top, Edge::Bottom] {
        inner.set_padding(edge, Some(Value::Point(1.0)));
    }
    for edge in [Edge::Left, Edge::Right] {
        inner.set_padding(edge, Some(Value::Point(2.0)));
    }
    bind_always(
        move || rgba(theme.with(|th| if hover.get() { th.decrease(th.background.raised.base, 1.0) } else { th.background.raised.base })),
        move |c| inner.set_background_color(Some(c)),
    );
    let summary = text();
    insert_node(inner, summary, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| summary.set_fg(Some(c)));
    {
        let count = count.clone();
        insert(summary, View::dynamic(move || {
            let n = count();
            View::from(format!("{n} message{} reverted", if n == 1 { "" } else { "s" }))
        }), At::Only);
    }
    let files_for = files.clone();
    insert(
        inner,
        show(
            move || !files_for().is_empty(),
            move || {
                let list = box_();
                list.set_padding(Edge::Top, Some(Value::Point(1.0)));
                list.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
                list.set_flex_direction(FlexDirection::Column);
                let files = files.clone();
                let width = ctx.width.clone();
                insert(
                    list,
                    for_each(
                        move || files(),
                        opencode_tui_sync::client::stringify,
                        move |file, _| {
                            let row = box_();
                            row.set_flex_direction(FlexDirection::Row);
                            row.set_gap(2, Some(Value::Point(1.0)));
                            row.set_flex_shrink(Some(0.0));
                            let status = text();
                            insert_node(row, status, None);
                            bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| status.set_fg(Some(c)));
                            insert(status, View::dynamic(move || View::from(status_label(file.get().str("status").unwrap_or_default()))), At::Only);
                            let width = width.clone();
                            let num = move |key: &str| file.get_untracked().num(key).unwrap_or(0.0);
                            let path = file_path(
                                Rc::new(move || file.get().str("file").unwrap_or_default().to_string()),
                                Rc::new(move || {
                                    let f = file.get();
                                    let additions = f.num("additions").unwrap_or(0.0);
                                    let deletions = f.num("deletions").unwrap_or(0.0);
                                    let a = if additions > 0.0 { string_width(&format!("+{additions}")) as i64 + 1 } else { 0 };
                                    let d = if deletions > 0.0 { string_width(&format!("-{deletions}")) as i64 + 1 } else { 0 };
                                    (width() as i64 - 7 - a - d).max(2)
                                }),
                                Rc::new(move || Some(rgba(theme.with(|th| th.text.base)))),
                            );
                            insert_node(row, path, None);
                            if num("additions") > 0.0 {
                                let added = text();
                                insert_node(row, added, None);
                                bind_always(move || rgba(theme.with(|th| th.diff.text.added)), move |c| added.set_fg(Some(c)));
                                insert(added, View::dynamic(move || View::from(format!("+{}", file.get().num("additions").unwrap_or(0.0)))), At::Only);
                            }
                            if num("deletions") > 0.0 {
                                let removed = text();
                                insert_node(row, removed, None);
                                bind_always(move || rgba(theme.with(|th| th.diff.text.removed)), move |c| removed.set_fg(Some(c)));
                                insert(removed, View::dynamic(move || View::from(format!("-{}", file.get().num("deletions").unwrap_or(0.0)))), At::Only);
                            }
                            View::Node(row)
                        },
                        None,
                    ),
                    At::Only,
                );
                View::Node(list)
            },
            None,
        ),
        At::End,
    );
    let hint = text();
    insert_node(inner, hint, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| hint.set_fg(Some(c)));
    let key = span();
    insert_node(hint, key, None);
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| key.set_span_style(Some(c), None, 0, 0));
    insert(key, View::dynamic(move || View::from(redo_key().unwrap_or_default())), At::Only);
    insert_node(hint, text_node_from_string(" or /redo to restore"), None);
    View::Node(outer)
}

/// `<QueuedPromptDock prompts onOpen>`.
pub fn queued_prompt_dock(prompts: Rc<dyn Fn() -> Vec<(String, String)>>, on_open: Rc<dyn Fn()>) -> View {
    let theme = tui().theme;
    let (hover, set_hover) = create_signal(false);
    let next = {
        let prompts = prompts.clone();
        create_memo(move || prompts().first().map(|(_, text)| text.replace('\n', " ")))
    };
    let outer = box_();
    outer.set_margin(Edge::Bottom, Some(Value::Point(1.0)));
    outer.set_border(BorderProp::Sides(vec![Side::Left]));
    bind_always(move || rgba(theme.with(|th| th.border.base)), move |c| outer.set_border_color(Some(c)));
    outer.set_custom_border_chars(Some(split_border_chars()));
    outer.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| set_hover.set(true))));
    outer.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| set_hover.set(false))));
    outer.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| on_open())));
    let inner = box_();
    insert_node(outer, inner, None);
    inner.set_width(Some(Value::Percent(100.0)));
    for edge in [Edge::Top, Edge::Bottom] {
        inner.set_padding(edge, Some(Value::Point(1.0)));
    }
    for edge in [Edge::Left, Edge::Right] {
        inner.set_padding(edge, Some(Value::Point(2.0)));
    }
    bind_always(
        move || rgba(theme.with(|th| if hover.get() { th.decrease(th.background.raised.base, 1.0) } else { th.background.raised.base })),
        move |c| inner.set_background_color(Some(c)),
    );
    inner.set_flex_direction(FlexDirection::Row);
    let label = text();
    insert_node(inner, label, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| label.set_fg(Some(c)));
    label.set_wrap_mode(WrapMode::None);
    label.set_truncate(true);
    label.set_flex_grow(Some(1.0));
    label.set_flex_shrink(Some(1.0));
    label.set_min_width(Some(Value::Point(0.0)));
    let count = span();
    insert_node(label, count, None);
    bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| count.set_span_style(Some(c), None, 0, 0));
    insert(count, View::dynamic(move || View::from(format!("{} queued", prompts().len()))), At::Only);
    insert(label, show_value(move || next.get(), |text| View::dynamic(move || View::from(format!(" · {}", text.get().unwrap_or_default()))), None), At::End);
    View::Node(outer)
}
