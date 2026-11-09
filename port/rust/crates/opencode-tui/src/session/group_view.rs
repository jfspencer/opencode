//! `routes/session/group-view.tsx`: grouped rows (thoughts, explorations, low-verbosity activity, instruction
//! loads) with their disclosure, and the pending (permission-blocked) tools kept outside a collapsed group.
use std::collections::HashMap;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, code};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, MouseHandler};
use opencode_tui_markup::js::JsString;
use opencode_tui_sync::json::JsonExt;

use super::activity_summary::summarize_activity;
use super::anchors::{entry_anchor, group_anchor, group_id, visit_entries};
use super::grouping::{Group, GroupKind, GroupNode, PartRef, SessionEntry, SessionGroup, SessionNode, instruction_paths};
use super::message_parts::{InlineToolRowProps, duration, inline_tool_row, reasoning_content, reasoning_summary, tool_display};
use super::parts::tool_images;
use super::render_context::{MarkdownMode, ThinkingMode, use_ctx};
use super::rows::resolve_part;
use crate::border::split_border_chars;
use crate::ctx::tui;
use crate::syntax::{current_syntax, thinking_syntax};
use crate::util::{fade, rgba};

/// `entry(entry, images)`: how a group renders one of its entries (SessionEntryView).
pub type EntryRenderer = Rc<dyn Fn(Rc<dyn Fn() -> Option<SessionEntry>>, Option<bool>) -> View>;

#[derive(Clone)]
struct GroupProps {
    node: Rc<dyn Fn() -> Group<SessionEntry>>,
    level: usize,
    completed: Rc<dyn Fn() -> bool>,
    pending: Rc<dyn Fn() -> Vec<PartRef>>,
    pending_outside: bool,
    images_outside: bool,
    entry: EntryRenderer,
}

fn message(session_id: &str, message_id: &str) -> Option<Rc<Json>> {
    tui().data.store.session.message.get(session_id, message_id)
}

/// `<SessionGroupView row entry images>`.
pub fn session_group_view(row: Rc<dyn Fn() -> Option<SessionGroup>>, entry: EntryRenderer) -> View {
    let r1 = row.clone();
    let r2 = row.clone();
    let r3 = row.clone();
    group(GroupProps {
        node: Rc::new(move || r1().map(|g| g.node()).unwrap_or(Group { kind: GroupKind::Activity, children: Vec::new(), size: 0 })),
        level: 0,
        completed: Rc::new(move || r2().is_some_and(|g| g.completed)),
        pending: Rc::new(move || r3().filter(|g| g.has_pending()).map(|g| g.pending).unwrap_or_default()),
        pending_outside: false,
        images_outside: false,
        entry,
    })
}

/// `<Group>`: keyed on the kind so kind-specific state stays isolated.
fn group(props: GroupProps) -> View {
    let node = props.node.clone();
    show_keyed(
        move || Some(node().kind),
        move |kind| match kind {
            GroupKind::Activity => activity_group(props.clone()),
            GroupKind::Instructions => instructions_group(props.clone()),
            _ => group_content(props.clone()),
        },
        None,
    )
}

fn descendants(node: &Group<SessionEntry>) -> Vec<SessionEntry> {
    let mut out = Vec::new();
    visit_entries(&node.children, &mut |entry| out.push(entry.clone()));
    out
}

fn is_pending(entry: &SessionEntry, pending: &[PartRef]) -> bool {
    matches!(entry, SessionEntry::Part { r#ref } if pending.contains(r#ref))
}

struct Disclosure {
    id: Memo<Option<String>>,
    expanded: Rc<dyn Fn() -> bool>,
    toggle: Rc<dyn Fn(Option<Element>)>,
}

/// `useDisclosure(props)`.
fn use_disclosure(props: &GroupProps) -> Disclosure {
    let ctx = use_ctx();
    let (node, level) = (props.node.clone(), props.level);
    let id = create_memo(move || group_id(&node(), level));
    let node = props.node.clone();
    let group_expanded = ctx.group_expanded.clone();
    let expanded: Rc<dyn Fn() -> bool> = Rc::new(move || match id.get() {
        Some(key) => group_expanded(&key, node().kind),
        None => false,
    });
    let e = expanded.clone();
    let set = ctx.set_group_expanded.clone();
    let toggle: Rc<dyn Fn(Option<Element>)> = Rc::new(move |anchor| {
        if super::render_context::has_selected_text() {
            return;
        }
        if let Some(key) = id.get_untracked() {
            set(&key, !untrack(|| e()), anchor);
        }
    });
    Disclosure { id, expanded, toggle }
}

fn hover_handlers(set: Signal<bool>) -> (MouseHandler, MouseHandler) {
    (Rc::new(move |_| set.set(true)), Rc::new(move |_| set.set(false)))
}

/// `<GroupContent>`: reasoning and exploration groups.
fn group_content(props: GroupProps) -> View {
    let ctx = use_ctx();
    let theme = tui().theme;
    let disclosure = use_disclosure(&props);
    let (hover, set_hover) = create_signal(false);
    let entries = {
        let node = props.node.clone();
        create_memo(move || descendants(&node()))
    };
    let refs = {
        let pending = props.pending.clone();
        create_memo(move || {
            let pending = pending();
            entries.get().into_iter().filter_map(|e| match e {
                SessionEntry::Part { r#ref } if !pending.contains(&r#ref) => Some(r#ref),
                _ => None,
            }).collect::<Vec<_>>()
        })
    };
    let session_id = ctx.session_id.clone();
    let kind = props.node.clone();
    let thoughts = {
        let (kind, session_id) = (kind.clone(), session_id.clone());
        create_memo_equals(
            move || {
                if kind().kind != GroupKind::Reasoning {
                    return Vec::new();
                }
                refs.get()
                    .iter()
                    .filter_map(|r| {
                        let m = message(&session_id, &r.message_id)?;
                        if m.str("type") != Some("assistant") {
                            return None;
                        }
                        let part = resolve_part(&m, &r.part_id)?;
                        (part.str("type") == Some("reasoning") && !reasoning_content(&part).is_empty()).then(|| (m, Rc::new(part)))
                    })
                    .collect::<Vec<_>>()
            },
            None,
        )
    };
    let tools = {
        let (kind, session_id) = (kind.clone(), session_id.clone());
        create_memo_equals(
            move || {
                if kind().kind != GroupKind::Exploration {
                    return Vec::new();
                }
                refs.get()
                    .iter()
                    .filter_map(|r| {
                        let m = message(&session_id, &r.message_id)?;
                        if m.str("type") != Some("assistant") {
                            return None;
                        }
                        let part = resolve_part(&m, &r.part_id)?;
                        (part.str("type") == Some("tool")).then(|| Rc::new(part))
                    })
                    .collect::<Vec<_>>()
            },
            None,
        )
    };
    let latest = create_memo_with(move |previous: Option<Option<String>>| {
        let previous = previous.flatten();
        let Some((message, part)) = thoughts.with(|t| t.last().cloned()) else { return previous };
        let (title, _) = reasoning_summary(&reasoning_content(&part));
        if title.is_some() {
            return title;
        }
        let done = part.get("time").and_then(|t| t.num("completed")).is_some() || message.get("time").and_then(|t| t.num("completed")).is_some();
        if done { None } else { previous }
    });
    let duration_ms = create_memo(move || {
        thoughts.with(|t| {
            t.iter().fold(0.0, |total, (_, part)| {
                let start = part.get("time").and_then(|t| t.num("created"));
                let end = part.get("time").and_then(|t| t.num("completed"));
                total + match (start, end) {
                    (Some(s), Some(e)) => (e - s).max(0.0),
                    _ => 0.0,
                }
            })
        })
    });
    let grouped = {
        let (kind, ctx) = (kind.clone(), ctx.clone());
        Rc::new(move || if kind().kind == GroupKind::Reasoning { (ctx.thinking_mode)() == ThinkingMode::Hide } else { (ctx.group_exploration)() })
    };
    let completed = {
        let (kind, props_completed) = (kind.clone(), props.completed.clone());
        Rc::new(move || {
            if kind().kind == GroupKind::Reasoning {
                return props_completed();
            }
            props_completed()
                || tools.with(|t| !t.is_empty() && t.iter().all(|p| p.get("time").and_then(|t| t.num("completed")).is_some()))
        })
    };
    let label = {
        let completed = completed.clone();
        create_memo(move || {
            let mut order: Vec<String> = Vec::new();
            let mut counts: HashMap<String, usize> = HashMap::new();
            for part in tools.get().iter() {
                let tool = tool_display(part.str("name").unwrap_or_default());
                let name = match tool {
                    "grep" | "glob" | "websearch" => "search",
                    "webfetch" => "fetch",
                    other => other,
                }
                .to_string();
                if !counts.contains_key(&name) {
                    order.push(name.clone());
                }
                *counts.entry(name).or_insert(0) += 1;
            }
            let names: Vec<String> = order
                .iter()
                .map(|name| {
                    let count = counts[name];
                    let noun = if count == 1 {
                        name.clone()
                    } else if name == "search" || name == "fetch" {
                        format!("{name}es")
                    } else {
                        format!("{name}s")
                    };
                    format!("{count} {noun}")
                })
                .collect();
            format!("{}: {}", if completed() { "Explored" } else { "Exploring" }, names.join(", "))
        })
    };
    let toggle = disclosure.toggle.clone();
    let expanded = disclosure.expanded.clone();
    let children = {
        let props = props.clone();
        Rc::new(move |mode: Mode| children_view(props.clone(), mode))
    };
    let (over, out) = hover_handlers(set_hover);
    let id = disclosure.id;
    let active = {
        let (grouped, kind) = (grouped.clone(), kind.clone());
        move || grouped() && if kind().kind == GroupKind::Reasoning { !thoughts.with(Vec::is_empty) } else { !tools.with(Vec::is_empty) }
    };
    let is_reasoning = {
        let kind = kind.clone();
        create_memo(move || kind().kind == GroupKind::Reasoning)
    };
    let exploration = {
        let (grouped, children, completed, expanded, toggle, props) =
            (grouped.clone(), children.clone(), completed.clone(), expanded.clone(), toggle.clone(), props.clone());
        let (over, out) = (over.clone(), out.clone());
        Rc::new(move || {
            let (children, completed, expanded, toggle, props) =
                (children.clone(), completed.clone(), expanded.clone(), toggle.clone(), props.clone());
            let (over, out) = (over.clone(), out.clone());
            let fallback_children = children.clone();
            let grouped = grouped.clone();
            show(
                move || grouped(),
                move || {
                    let (completed, toggle) = (completed.clone(), toggle.clone());
                    let (over, out) = (over.clone(), out.clone());
                    let summary = show(
                        move || !tools.with(Vec::is_empty),
                        move || {
                            let (c1, c2, c3) = (completed.clone(), completed.clone(), completed.clone());
                            let toggle = toggle.clone();
                            inline_tool_row(InlineToolRowProps {
                                icon: Rc::new(move || if c1() { "→".into() } else { "✱".into() }),
                                color: Some(Rc::new(move || Some(rgba(theme.with(|th| if hover.get() { th.text.base } else { th.text.muted }))))),
                                complete: Rc::new(move || c2()),
                                pending: Rc::new(move || label.get()),
                                spinner: Rc::new(move || !c3()),
                                on_mouse_over: Some(over.clone()),
                                on_mouse_out: Some(out.clone()),
                                on_mouse_up: Some(Rc::new(move |_| toggle(None))),
                                children: Rc::new(move || View::from(label.get())),
                                ..InlineToolRowProps::default()
                            })
                        },
                        None,
                    );
                    let expanded = expanded.clone();
                    let children = children.clone();
                    let body = show(move || expanded() && !tools.with(Vec::is_empty), move || children(Mode::Tool), None);
                    let images = if props.images_outside {
                        View::Empty
                    } else {
                        tool_images(Rc::new(move || tools.get()))
                    };
                    View::list([summary, body, images])
                },
                Some(Rc::new(move || fallback_children(Mode::Normal))),
            )
        })
    };
    let reasoning = {
        let (grouped, children, expanded, toggle, props) = (grouped.clone(), children.clone(), expanded.clone(), toggle.clone(), props.clone());
        Rc::new(move || {
            let (grouped, children, expanded, toggle, props) = (grouped.clone(), children.clone(), expanded.clone(), toggle.clone(), props.clone());
            let (over, out) = (over.clone(), out.clone());
            show(
                move || !thoughts.with(Vec::is_empty),
                move || {
                    let (children, expanded, toggle, props) = (children.clone(), expanded.clone(), toggle.clone(), props.clone());
                    let fallback = children.clone();
                    let (over, out) = (over.clone(), out.clone());
                    let grouped = grouped.clone();
                    show(
                        move || grouped(),
                        move || {
                            let (e1, e2) = (expanded.clone(), expanded.clone());
                            let (p1, p2, p3, p4) = (props.completed.clone(), props.completed.clone(), props.completed.clone(), props.completed.clone());
                            let toggle = toggle.clone();
                            let row = inline_tool_row(InlineToolRowProps {
                                icon: Rc::new(move || if e1() { "-".into() } else { "+".into() }),
                                color: Some(Rc::new(move || {
                                    let warning = rgba(theme.with(|th| th.text.feedback.warning.base));
                                    Some(if !p1() {
                                        rgba(theme.with(|th| th.text.base))
                                    } else if hover.get() || e2() {
                                        warning
                                    } else {
                                        fade(warning, 0.6)
                                    })
                                })),
                                complete: Rc::new(move || p2()),
                                pending: Rc::new(move || latest.get().map_or("Thinking".into(), |l| format!("Thinking: {l}"))),
                                spinner: Rc::new(move || !p3()),
                                on_mouse_over: Some(over.clone()),
                                on_mouse_out: Some(out.clone()),
                                on_mouse_up: Some(Rc::new(move |_| toggle(None))),
                                children: {
                                    let expanded = expanded.clone();
                                    Rc::new(move || {
                                        let done = p4();
                                        let mut text = if done {
                                            "Thought".to_string()
                                        } else {
                                            latest.get().map_or("Thinking".into(), |l| format!("Thinking: {l}"))
                                        };
                                        if done && !expanded() {
                                            if let Some(latest) = latest.get() {
                                                text.push_str(&format!(": {latest}"));
                                            }
                                        }
                                        let count = thoughts.with(Vec::len);
                                        if done && count > 1 {
                                            text.push_str(&format!(" · {count} steps"));
                                        }
                                        if done && duration_ms.get() != 0.0 {
                                            text.push_str(&format!(" · {}", duration(duration_ms.get())));
                                        }
                                        View::from(text)
                                    })
                                },
                                ..InlineToolRowProps::default()
                            });
                            let (expanded, children) = (expanded.clone(), children.clone());
                            let body = show(
                                move || expanded(),
                                move || {
                                    let wrap = box_();
                                    wrap.set_padding(Edge::Left, Some(Value::Point(3.0)));
                                    insert(wrap, children(Mode::Thought), At::Only);
                                    View::Node(wrap)
                                },
                                None,
                            );
                            View::list([row, body])
                        },
                        Some(Rc::new(move || fallback(Mode::Normal))),
                    )
                },
                None,
            )
        })
    };
    let content = show(move || is_reasoning.get(), move || reasoning(), Some(Rc::new(move || exploration())));
    let pending = pending_entries(props.clone(), entries);
    group_anchor(move || id.get(), active, View::list([content, pending]))
}

/// Low verbosity: one summary for a run of tools, thoughts and instruction loads.
fn activity_group(props: GroupProps) -> View {
    let ctx = use_ctx();
    let theme = tui().theme;
    let disclosure = use_disclosure(&props);
    let (hover, set_hover) = create_signal(false);
    let entries = {
        let node = props.node.clone();
        create_memo(move || descendants(&node()))
    };
    let summary = {
        let (node, pending, completed, session_id) = (props.node.clone(), props.pending.clone(), props.completed.clone(), ctx.session_id.clone());
        create_memo(move || summarize_activity(&node(), &|id: &str| message(&session_id, id), &pending(), completed()))
    };
    let (over, out) = hover_handlers(set_hover);
    let id = disclosure.id;
    let expanded = disclosure.expanded.clone();
    let toggle = disclosure.toggle.clone();
    let props_for = props.clone();
    let body = show(
        move || !summary.with(|s| s.0.is_empty()),
        move || {
            let (e1, e2) = (expanded.clone(), expanded.clone());
            let toggle = toggle.clone();
            let row = inline_tool_row(InlineToolRowProps {
                icon: Rc::new(move || if e1() { "−".into() } else { "+".into() }),
                color: Some(Rc::new(move || Some(rgba(theme.with(|th| if hover.get() { th.text.base } else { th.text.muted }))))),
                complete: Rc::new(|| true),
                pending: Rc::new(move || summary.with(|s| s.0.clone())),
                spinner: Rc::new(move || !e2() && summary.with(|s| s.1)),
                on_mouse_over: Some(over.clone()),
                on_mouse_out: Some(out.clone()),
                on_mouse_up: Some(Rc::new(move |event| toggle(event.current_target))),
                children: Rc::new(move || View::from(summary.with(|s| s.0.clone()))),
                ..InlineToolRowProps::default()
            });
            let (expanded, props) = (expanded.clone(), props_for.clone());
            let open = show(
                move || expanded(),
                move || {
                    let column = box_();
                    column.set_flex_direction(FlexDirection::Column);
                    column.set_gap(2, Some(Value::Point(1.0)));
                    column.set_margin(Edge::Top, Some(Value::Point(1.0)));
                    insert(column, children_view(props.clone(), Mode::Normal), At::Only);
                    View::Node(column)
                },
                None,
            );
            View::list([row, open])
        },
        None,
    );
    let pending = pending_entries(props.clone(), entries);
    group_anchor(move || id.get(), move || !summary.with(|s| s.0.is_empty()), View::list([body, pending]))
}

/// Consecutive instruction loads, summarized by the number of distinct files.
fn instructions_group(props: GroupProps) -> View {
    let ctx = use_ctx();
    let theme = tui().theme;
    let disclosure = use_disclosure(&props);
    let (hover, set_hover) = create_signal(false);
    let files = {
        let (node, session_id) = (props.node.clone(), ctx.session_id.clone());
        create_memo(move || {
            let mut seen = std::collections::HashSet::new();
            for entry in descendants(&node()) {
                if let SessionEntry::Message { message_id } = entry {
                    for path in instruction_paths(message(&session_id, &message_id).as_deref()) {
                        seen.insert(path);
                    }
                }
            }
            seen.len()
        })
    };
    let (over, out) = hover_handlers(set_hover);
    let toggle = disclosure.toggle.clone();
    let row = inline_tool_row(InlineToolRowProps {
        icon: Rc::new(|| "◈".into()),
        color: Some(Rc::new(move || Some(rgba(theme.with(|th| if hover.get() { th.text.base } else { th.text.muted }))))),
        complete: Rc::new(|| true),
        pending: Rc::new(String::new),
        on_mouse_over: Some(over),
        on_mouse_out: Some(out),
        on_mouse_up: Some(Rc::new(move |_| toggle(None))),
        children: Rc::new(move || {
            let n = files.get();
            View::from(format!("Instructions: {n} {}", if n == 1 { "file" } else { "files" }))
        }),
        ..InlineToolRowProps::default()
    });
    let expanded = disclosure.expanded.clone();
    let props_for = props.clone();
    let body = show(move || expanded(), move || children_view(props_for.clone(), Mode::Normal), None);
    let id = disclosure.id;
    group_anchor(move || id.get(), move || files.get() > 0, View::list([row, body]))
}

/// Permission-blocked tools stay visible below the root group, even when collapsed.
fn pending_entries(props: GroupProps, entries: Memo<Vec<SessionEntry>>) -> View {
    if props.pending_outside {
        return View::Empty;
    }
    let pending = props.pending.clone();
    let entry = props.entry.clone();
    for_each(
        move || pending(),
        |r: &PartRef| r.clone(),
        move |r, _| {
            let leaf = create_memo(move || {
                let r = r.get();
                entries.with(|e| e.iter().find(|entry| matches!(entry, SessionEntry::Part { r#ref } if *r#ref == r)).cloned())
            });
            let entry = entry.clone();
            show_value(
                move || leaf.get(),
                move |item| {
                    let current = item.get_untracked().expect("leaf");
                    entry_anchor(current, None, entry(Rc::new(move || item.get()), None))
                },
                None,
            )
        },
        None,
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    Thought,
    Tool,
}

/// `<Children nodes mode>`.
fn children_view(props: GroupProps, mode: Mode) -> View {
    let node = props.node.clone();
    let nodes = create_memo_equals(move || node().children, None);
    let indexed = move || nodes.get().into_iter().enumerate().collect::<Vec<(usize, SessionNode)>>();
    let props_for = props.clone();
    index(
        indexed,
        move |item, _| {
            let props = props_for.clone();
            let is_group = create_memo(move || matches!(item.get().1, GroupNode::Group(_)));
            let (p1, p2) = (props.clone(), props.clone());
            switch(
                vec![
                    when(move || is_group.get(), move || {
                        let props = p1.clone();
                        let child = create_memo_equals(
                            move || match item.get().1 {
                                GroupNode::Group(g) => g,
                                _ => Group { kind: GroupKind::Activity, children: Vec::new(), size: 0 },
                            },
                            None,
                        );
                        let parent_completed = props.completed.clone();
                        let pending = props.pending.clone();
                        let session_id = use_ctx().session_id.clone();
                        group(GroupProps {
                            node: Rc::new(move || child.get()),
                            level: props.level + 1,
                            completed: Rc::new(move || {
                                let (index, _) = item.get();
                                let pending = pending();
                                parent_completed()
                                    || nodes.with(|n| {
                                        n.iter().skip(index + 1).any(|next| match next {
                                            GroupNode::Group(_) => true,
                                            GroupNode::Entry(entry) => !is_pending(entry, &pending),
                                        })
                                    })
                                    || (child.with(|c| c.kind) == GroupKind::Reasoning
                                        && child.with(|c| reasoning_completed(&c.children, &session_id)))
                            }),
                            pending: props.pending.clone(),
                            pending_outside: true,
                            images_outside: props.images_outside || mode == Mode::Tool,
                            entry: props.entry.clone(),
                        })
                    }),
                    when(move || !is_group.get(), move || {
                        let props = p2.clone();
                        let entry = create_memo(move || match item.get().1 {
                            GroupNode::Entry(e) => Some(e),
                            _ => None,
                        });
                        let pending = props.pending.clone();
                        show(
                            move || !entry.get().is_some_and(|e| is_pending(&e, &pending())),
                            move || {
                                let Some(current) = entry.get_untracked() else { return View::Empty };
                                if mode == Mode::Thought {
                                    return thought_entry(current);
                                }
                                let images = if mode == Mode::Tool { Some(false) } else { None };
                                entry_anchor(current, None, (props.entry)(Rc::new(move || entry.get()), images))
                            },
                            None,
                        )
                    }),
                ],
                None,
            )
        },
        None,
    )
}

/// `<ThoughtEntry entry>`.
fn thought_entry(entry: SessionEntry) -> View {
    let ctx = use_ctx();
    let theme = tui().theme;
    let thinking = create_memo_equals(move || Rc::new(thinking_syntax(&current_syntax(), rgba(theme.with(|th| th.text.muted)))), None);
    let SessionEntry::Part { r#ref } = entry.clone() else { return View::Empty };
    let session_id = ctx.session_id.clone();
    let message_memo = {
        let (session_id, r) = (session_id.clone(), r#ref.clone());
        create_memo_equals(
            move || message(&session_id, &r.message_id).filter(|m| m.str("type") == Some("assistant")),
            Some(super::parts::same_record),
        )
    };
    let part = {
        let r = r#ref.clone();
        create_memo(move || {
            let m = message_memo.get()?;
            resolve_part(&m, &r.part_id).filter(|p| p.str("type") == Some("reasoning"))
        })
    };
    let content = create_memo(move || part.get().map(|p| reasoning_content(&p)).unwrap_or_default());
    let markdown_mode = ctx.markdown_mode.clone();
    show(
        move || !content.get().is_empty(),
        move || {
            let wrap = box_();
            wrap.set_border(BorderProp::Sides(vec![Side::Left]));
            wrap.set_custom_border_chars(Some(split_border_chars()));
            bind_always(move || rgba(theme.with(|th| th.decrease(th.background.raised.base, 1.0))), move |c: RGBA| wrap.set_border_color(Some(c)));
            wrap.set_padding(Edge::Left, Some(Value::Point(1.0)));
            let block = code();
            insert_node(wrap, block, None);
            block.set_filetype(Some("markdown".into()));
            block.set_draw_unstyled_text(false);
            bind(
                move || {
                    part.get().and_then(|p| p.get("time").and_then(|t| t.num("completed"))).is_none()
                        && message_memo.get().and_then(|m| m.get("time").and_then(|t| t.num("completed"))).is_none()
                },
                move |s| block.set_streaming(s),
            );
            bind_always(move || thinking.get(), move |s| block.set_syntax_style(s));
            bind(move || content.get(), move |c| block.set_code_content(JsString::from(c.as_str())));
            let markdown_mode = markdown_mode.clone();
            bind(move || markdown_mode() == MarkdownMode::Rendered, move |c| block.set_conceal(c));
            bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| block.set_fg(Some(c)));
            entry_anchor(entry.clone(), Some(1.0), View::Node(wrap))
        },
        None,
    )
}

fn reasoning_completed(nodes: &[SessionNode], session_id: &str) -> bool {
    nodes.iter().all(|node| match node {
        GroupNode::Group(g) => reasoning_completed(&g.children, session_id),
        GroupNode::Entry(SessionEntry::Part { r#ref }) => {
            let Some(m) = message(session_id, &r#ref.message_id) else { return false };
            if m.str("type") != Some("assistant") {
                return false;
            }
            resolve_part(&m, &r#ref.part_id)
                .is_some_and(|p| p.str("type") == Some("reasoning") && p.get("time").and_then(|t| t.num("completed")).is_some())
        }
        GroupNode::Entry(_) => false,
    })
}
