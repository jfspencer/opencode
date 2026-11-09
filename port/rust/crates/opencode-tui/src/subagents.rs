//! The composer's subagents tab (`routes/session/composer/subagents-tab.tsx`): the session family as a tree
//! (`util/session.ts`'s `sessionFamily`), filtered to active or inactive sessions, navigated with the composer's
//! keys; and `@opencode/util/session-title-fallback`.
//!
//! The composer (index.tsx, the prompt slice's) hands the tab its context (`useComposerTab()`) as
//! [`ComposerTabApi`]. TODO(port-tui): the tabs slice's `crate::storage` keeps the filter
//! (`memory("subagents-filter")`); until it lands the filter is a thread-local signal with the same lifetime.
use std::cell::Cell;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, scrollbox, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::scrollbox::ScrollBarOptions;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, Element, FlexDirection, MouseEventType};
use opencode_tui_sync::data::SessionStatus;
use opencode_tui_sync::json::JsonExt;

use crate::ctx::{Route, tui};
use crate::keymap::{Command, Layer, create_layer, use_shortcuts};
use crate::util::{rgba, titlecase};

/// `ComposerHint`.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposerHint {
    pub label: String,
    pub shortcut: String,
}

/// `ComposerTab`.
#[derive(Clone)]
pub struct ComposerTab {
    pub id: &'static str,
    pub label: &'static str,
    pub hints: Option<Rc<dyn Fn() -> Vec<ComposerHint>>>,
}

/// `useComposerTab()`: what the composer gives its tabs.
#[derive(Clone)]
pub struct ComposerTabApi {
    pub register: Rc<dyn Fn(ComposerTab) -> Box<dyn FnOnce()>>,
    pub active: Rc<dyn Fn(&str) -> bool>,
    pub close: Rc<dyn Fn()>,
}

/// An ISO-8601 UTC timestamp of epoch milliseconds (`new Date(ms).toISOString()`).
pub fn iso_string(ms: f64) -> String {
    let ms = ms as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

/// `withTimestampedFallback(info)`.
pub fn with_timestamped_fallback(info: &Json) -> String {
    if let Some(title) = info.str("title") {
        return title.to_string();
    }
    let kind = if info.str("parentID").is_some() { "Child" } else { "New" };
    format!("{kind} session - {}", iso_string(info.get("time").and_then(|t| t.num("created")).unwrap_or(0.0)))
}

/// `sessionFamily(sessions, sessionID)`: the tree of the session's root, depth first, with the branch prefixes.
pub fn session_family(sessions: &[Rc<Json>], session_id: &str) -> Vec<(Rc<Json>, String)> {
    let by_id = |id: &str| sessions.iter().find(|s| s.str("id") == Some(id)).cloned();
    let Some(current) = by_id(session_id) else { return Vec::new() };
    let children = |parent: &str| -> Vec<Rc<Json>> {
        sessions.iter().filter(|s| s.str("parentID").filter(|p| !p.is_empty()) == Some(parent)).cloned().collect()
    };
    let mut root = current;
    while let Some(parent) = root.str("parentID").filter(|p| !p.is_empty()).and_then(|p| by_id(p)) {
        root = parent;
    }
    fn walk(
        parent: &str,
        ancestors: &[bool],
        children: &dyn Fn(&str) -> Vec<Rc<Json>>,
        out: &mut Vec<(Rc<Json>, String)>,
    ) {
        let group = children(parent);
        for (index, session) in group.iter().enumerate() {
            let last = index == group.len() - 1;
            let prefix = if ancestors.is_empty() {
                String::new()
            } else {
                let mut p: String = ancestors[1..].iter().map(|a| if *a { "   " } else { "│  " }).collect();
                p.push_str(if last { "└─ " } else { "├─ " });
                p
            };
            out.push((session.clone(), prefix));
            let mut next = ancestors.to_vec();
            next.push(last);
            walk(session.str("id").unwrap_or(""), &next, children, out);
        }
    }
    let mut out = Vec::new();
    walk(root.str("id").unwrap_or(""), &[], &children, &mut out);
    out
}

/// `SubagentEntry`.
#[derive(Clone, Debug, PartialEq)]
struct SubagentEntry {
    session_id: String,
    agent: String,
    title: String,
    running: bool,
    current: bool,
    prefix: String,
}

thread_local! {
    static FILTER_ACTIVE: Cell<Option<Signal<bool>>> = const { Cell::new(None) };
}

/// The filter's TUI-lifetime memory (`useStorage().memory("subagents-filter", { initial: { active: true } })`).
fn filter_active() -> Signal<bool> {
    FILTER_ACTIVE.with(|f| match f.get() {
        Some(signal) if signal.is_alive() => signal,
        _ => {
            let signal = create_root(|_| create_signal(true).0);
            f.set(Some(signal));
            signal
        }
    })
}

/// `@(\w+) subagent`: the agent name and the match's byte range.
fn agent_match(title: &str) -> Option<(String, std::ops::Range<usize>)> {
    let bytes = title.as_bytes();
    let mut search = 0;
    while let Some(offset) = title[search..].find('@') {
        let at = search + offset;
        let mut end = at + 1;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if end > at + 1 && title[end..].starts_with(" subagent") {
            return Some((title[at + 1..end].to_string(), at..end + " subagent".len()));
        }
        search = at + 1;
    }
    None
}

pub struct SubagentsTabProps {
    pub session_id: String,
    pub composer: ComposerTabApi,
}

/// `<SubagentsTab sessionID>`.
pub fn subagents_tab(props: SubagentsTabProps) -> View {
    let t = tui();
    let theme = t.theme;
    let route = t.route;
    let store = t.data.store.clone();
    let client = t.data.client_context.clone();
    let composer = props.composer.clone();
    let shortcuts = use_shortcuts();
    let session_id = props.session_id.clone();
    let session = {
        let store = store.clone();
        let id = session_id.clone();
        create_memo_equals(move || store.session.get(&id), None)
    };
    let (selected, set_selected) = create_signal(0usize);
    let filter = filter_active();
    let route_session = move || route.with(|r| match r {
        Route::Session { session_id } => session_id.clone(),
        _ => String::new(),
    });

    let entries = {
        let store = store.clone();
        create_memo(move || {
            let Some(current) = session.get() else { return Vec::new() };
            let list = store.session.list();
            let route_id = route_session();
            let active = filter.get();
            session_family(&list, current.str("id").unwrap_or(""))
                .into_iter()
                .map(|(session, prefix)| {
                    let title = with_timestamped_fallback(&session);
                    let matched = agent_match(&title);
                    let agent = match (session.str("agent").filter(|a| !a.is_empty()), &matched) {
                        (Some(agent), _) => titlecase(agent),
                        (None, Some((name, _))) => titlecase(name),
                        (None, None) => "Subagent".into(),
                    };
                    let title = match &matched {
                        Some((_, range)) => {
                            let stripped = format!("{}{}", &title[..range.start], &title[range.end..]);
                            let trimmed = opencode_tui_sync::js::trim(&stripped).to_string();
                            if trimmed.is_empty() { title.clone() } else { trimmed }
                        }
                        None => title,
                    };
                    let id = session.str("id").unwrap_or("").to_string();
                    SubagentEntry {
                        running: store.session.status(&id) == SessionStatus::Running,
                        current: id == route_id,
                        session_id: id,
                        agent,
                        title,
                        prefix,
                    }
                })
                .filter(|entry| if active { entry.running } else { !entry.running })
                .collect::<Vec<_>>()
        })
    };
    let scroll: Rc<Cell<Option<Element>>> = Rc::new(Cell::new(None));
    let selected_entry = create_memo(move || entries.with(|e| e.get(selected.get()).cloned()));

    let scroll_to_index = {
        let scroll = scroll.clone();
        Rc::new(move |index: usize, center: bool| {
            let Some(scroll) = scroll.get() else { return };
            let height = scroll.scroll_viewport().height as f64;
            let index = index as f64;
            if center {
                scroll.scroll_to(0f64.max(index - (height / 2.0).floor()));
                return;
            }
            if index >= scroll.scroll_top() + height {
                scroll.scroll_to(index - height + 1.0);
            }
            if index < scroll.scroll_top() {
                scroll.scroll_to(index);
                if index == 0.0 {
                    scroll.scroll_to(0.0);
                }
            }
        })
    };
    let move_to = {
        let scroll_to_index = scroll_to_index.clone();
        Rc::new(move |next: usize, center: bool| {
            set_selected.set(next);
            scroll_to_index(next, center);
        })
    };

    {
        let composer = composer.clone();
        let scroll_to_index = scroll_to_index.clone();
        let move_to = move_to.clone();
        let selected_session = Rc::new(std::cell::RefCell::new(String::new()));
        let was_active = Rc::new(Cell::new(false));
        create_effect(move || {
            let active = (composer.active)("subagents");
            if !active {
                if was_active.get() {
                    selected_session.borrow_mut().clear();
                    set_selected.set(0);
                }
                was_active.set(false);
                return;
            }
            let list = entries.get();
            let route_id = route_session();
            if *selected_session.borrow() != route_id && !list.is_empty() {
                let next = list.iter().position(|e| e.current).unwrap_or(0);
                *selected_session.borrow_mut() = route_id;
                set_selected.set(next);
                let scroll_to_index = scroll_to_index.clone();
                let scroll_current = Rc::new(move || scroll_to_index(next, true));
                scroll_current();
                // The remounted scroll box finishes layout on the next frame and resets its scroll position.
                let again = scroll_current.clone();
                renderer::on_frame(
                    move || {
                        let again = again.clone();
                        renderer::on_frame(move || again(), true);
                    },
                    true,
                );
                renderer::request_render();
            }
            was_active.set(true);
            if selected.get_untracked() >= list.len() {
                move_to(list.len().saturating_sub(1), false);
            }
        });
    }

    {
        let composer = composer.clone();
        let shortcuts = shortcuts.clone();
        on_mount(move || {
            let shortcuts = shortcuts.clone();
            let cleanup = (composer.register)(ComposerTab {
                id: "subagents",
                label: "Subagents",
                hints: Some(Rc::new(move || {
                    let mut hints = Vec::new();
                    if selected_entry.with(|e| e.as_ref().is_some_and(|e| e.running)) {
                        hints.push(ComposerHint {
                            label: "interrupt".into(),
                            shortcut: shortcuts.get("composer.subagent.interrupt").unwrap_or_default(),
                        });
                    }
                    hints.push(ComposerHint {
                        label: format!("show {}", if filter.get() { "inactive" } else { "active" }),
                        shortcut: shortcuts.get("composer.subagent.toggle-activity").unwrap_or_default(),
                    });
                    hints
                })),
            });
            on_cleanup(cleanup);
        });
    }

    {
        let composer = composer.clone();
        let move_to = move_to.clone();
        let scroll = scroll.clone();
        create_layer(move || {
            let active = composer.active.clone();
            let close = composer.close.clone();
            let up = {
                let move_to = move_to.clone();
                move || {
                    if selected.get_untracked() == 0 {
                        close();
                        return;
                    }
                    move_to(selected.get_untracked() - 1, true);
                }
            };
            let down = {
                let move_to = move_to.clone();
                move || {
                    let len = entries.with_untracked(Vec::len);
                    if len == 0 {
                        return;
                    }
                    move_to((selected.get_untracked() + 1) % len, true);
                }
            };
            let select = move || {
                if let Some(entry) = entries.with_untracked(|e| e.get(selected.get_untracked()).cloned()) {
                    route.set(Route::Session { session_id: entry.session_id });
                }
            };
            let toggle = {
                let scroll = scroll.clone();
                move || {
                    filter.update(|f| *f = !*f);
                    set_selected.set(0);
                    if let Some(scroll) = scroll.get() {
                        scroll.scroll_to(0.0);
                    }
                }
            };
            let interrupt = {
                let client = client.clone();
                move || {
                    let Some(entry) = selected_entry.get_untracked() else { return };
                    if !entry.running {
                        return;
                    }
                    let call = client.api().call_named(
                        "session.interrupt",
                        &opencode_tui_sync::obj! { "sessionID" => entry.session_id.as_str() },
                        opencode_tui_sync::client::RequestOptions::default(),
                    );
                    tokio::task::spawn_local(async move {
                        let _ = call.await;
                    });
                }
            };
            Layer {
                mode: Some("composer"),
                enabled: Some(crate::keymap::Enabled::Fn(Rc::new(move || active("subagents")))),
                priority: Some(1),
                commands: vec![
                    Command::new("composer.subagent.up", up).title("Previous subagent").group("Composer"),
                    Command::new("composer.subagent.down", down).title("Next subagent").group("Composer"),
                    Command::new("composer.subagent.select", select).title("Navigate to subagent").group("Composer"),
                    Command::new("composer.subagent.toggle-activity", toggle)
                        .title("Toggle active subagents")
                        .group("Composer")
                        .bind(crate::keymap::Bind::Key("ctrl+a".into())),
                    Command::new("composer.subagent.interrupt", interrupt).title("Interrupt subagent").group("Composer"),
                ],
                ..Layer::default()
            }
        });
    }

    let active = composer.active.clone();
    show(
        move || active("subagents"),
        move || {
            let el = scrollbox();
            el.set_scrollbar_options(None, &ScrollBarOptions { visible: Some(Some(false)), ..Default::default() });
            el.set_max_height(Some(Value::Point(5.0)));
            scroll.set(Some(el));
            let empty = Rc::new(move || {
                let label = text();
                bind_always(move || rgba(theme.with(|t| t.text.muted)), move |c| label.set_fg(Some(c)));
                insert(
                    label,
                    View::dynamic(move || View::from(format!(" No {} subagents", if filter.get() { "active" } else { "inactive" }))),
                    At::Only,
                );
                View::Node(label)
            });
            insert(
                el,
                show(
                    move || entries.with(|e| !e.is_empty()),
                    move || {
                        for_each(
                            move || entries.get(),
                            |e: &SubagentEntry| e.session_id.clone(),
                            move |entry, index| {
                                let is_active = create_memo(move || index.get() == selected.get());
                                let row = box_();
                                row.set_flex_direction(FlexDirection::Row);
                                row.set_padding(Edge::Left, Some(Value::Point(1.0)));
                                row.set_padding(Edge::Right, Some(Value::Point(1.0)));
                                bind_always(
                                    move || {
                                        theme.with(|t| {
                                            if is_active.get() {
                                                rgba(t.background.action.primary.focused)
                                            } else if entry.with(|e| e.current) {
                                                rgba(t.background.action.primary.selected)
                                            } else {
                                                rgba(t.background.action.primary.base)
                                            }
                                        })
                                    },
                                    move |c| row.set_background_color(Some(c)),
                                );
                                row.set_on_mouse(Some(MouseEventType::Move), Some(Rc::new(move |_| set_selected.set(index.get_untracked()))));
                                row.set_on_mouse(
                                    Some(MouseEventType::Up),
                                    Some(Rc::new(move |_| {
                                        set_selected.set(index.get_untracked());
                                        route.set(Route::Session { session_id: entry.with_untracked(|e| e.session_id.clone()) });
                                    })),
                                );
                                let name_box = box_();
                                insert_node(row, name_box, None);
                                name_box.set_flex_grow(Some(1.0));
                                name_box.set_min_width(Some(Value::Point(0.0)));
                                name_box.set_flex_direction(FlexDirection::Row);
                                let name = text();
                                insert_node(name_box, name, None);
                                bind_always(
                                    move || {
                                        theme.with(|t| {
                                            if is_active.get() {
                                                rgba(t.text.action.primary.focused)
                                            } else if entry.with(|e| e.current) {
                                                rgba(t.text.action.primary.selected)
                                            } else {
                                                rgba(t.text.action.primary.base)
                                            }
                                        })
                                    },
                                    move |c| name.set_fg(Some(c)),
                                );
                                bind(move || is_active.get(), move |a| name.set_attributes(a.then_some(text_attributes::BOLD)));
                                name.set_wrap_mode(WrapMode::None);
                                insert(
                                    name,
                                    View::dynamic(move || entry.with(|e| View::from(format!("{}{}: {}", e.prefix, e.agent, e.title)))),
                                    At::Only,
                                );
                                insert(
                                    row,
                                    show(
                                        move || entry.with(|e| e.running),
                                        move || {
                                            let status = text();
                                            bind_always(
                                                move || theme.with(|t| if is_active.get() { rgba(t.text.action.primary.focused) } else { rgba(t.text.muted) }),
                                                move |c| status.set_fg(Some(c)),
                                            );
                                            status.set_wrap_mode(WrapMode::None);
                                            insert(status, View::from("Running"), At::Only);
                                            View::Node(status)
                                        },
                                        None,
                                    ),
                                    At::End,
                                );
                                View::Node(row)
                            },
                            None,
                        )
                    },
                    Some(empty),
                ),
                At::Only,
            );
            View::Node(el)
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(iso_string(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_string(1_767_225_600_123.0), "2026-01-01T00:00:00.123Z");
        let (name, range) = agent_match("Survey the docs (@explore subagent)").unwrap();
        assert_eq!(name, "explore");
        assert_eq!(&"Survey the docs (@explore subagent)"[range], "@explore subagent");
    }
}
