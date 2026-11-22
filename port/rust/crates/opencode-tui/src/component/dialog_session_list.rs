//! `component/dialog-session-list.tsx`: every top-level session of the open project (or of every project), with
//! the server-backed search behind typing, the pinned group and quick-switch slots without tabs, the project in the
//! title, the date groups, and the pin, delete (twice to confirm) and rename actions.
//!
//! `sessionTabs.enabled()` hides the pin action and the slot gutter while tabs are enabled, and the tabs slice
//! supplies each session's attention marker, as in TS.
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection};
use opencode_tui_sync::data::SessionStatus;

use crate::component::dialog_session_rename;
use crate::ctx::tui;
use crate::data::{array, get, get_str};
use crate::keymap::{Command, use_shortcuts};
use crate::spinner::{SpinnerProps, spinner};
use crate::theme::{ResolvedTheme, SurfaceName};
use crate::ui::dialog::{DIALOG_LIST_INSET, DialogSize, use_dialog};
use crate::ui::dialog_select::{FooterHint, SelectAction, SelectOption, SelectProps, Side, dialog_select};
use crate::ui::locale;
use crate::ui::theme::use_theme;
use crate::util::rgba;

fn point(v: f32) -> Option<Value> {
    Some(Value::Point(v))
}

fn js_basename(value: &str) -> String {
    std::path::Path::new(value)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `path.basename(relative)` in the session-row footer.
fn relative_start(value: &str) -> bool {
    value == ".." || value.starts_with("../") || value.starts_with('/')
}

/// `withTimestampedFallback(info)`.
pub(crate) fn with_timestamped_fallback(session: &Json) -> String {
    if let Some(title) = get_str(session, "title") {
        return title.to_string();
    }
    let parent = get_str(session, "parentID").is_some_and(|p| !p.is_empty());
    let created = get(session, "time").and_then(|t| get(t, "created")).and_then(|v| match v {
        Json::Number(n) => Some(*n),
        _ => None,
    });
    let stamp = created.and_then(crate::usage_limits::iso_string).unwrap_or_else(|| "Invalid Date".into());
    format!("{} session - {stamp}", if parent { "Child" } else { "New" })
}

/// `projectName(project, fallback)`.
pub(crate) fn project_name(project: Option<&Json>, fallback: &str) -> Option<String> {
    let canonical = project.and_then(|p| get_str(p, "canonical")).unwrap_or(fallback);
    if canonical == "/" {
        return (!fallback.is_empty()).then(|| js_basename(fallback));
    }
    let name = project.and_then(|p| get_str(p, "name")).filter(|name| !name.is_empty());
    Some(name.map(str::to_string).unwrap_or_else(|| js_basename(canonical)))
}

/// `theme.hue.accent[200]`.
fn accent_200(theme: &ResolvedTheme) -> RGBA {
    theme.hue.get("accent").and_then(|scale| scale.step(200)).map(rgba).unwrap_or(rgba(theme.text.base))
}

/// `sessionStatusGutter(theme, attention, running, fallback)`.
pub(crate) fn session_status_gutter(
    theme: Rc<dyn Fn() -> ResolvedTheme>,
    attention: Option<&'static str>,
    running: bool,
    fallback: Option<Rc<dyn Fn(RGBA) -> View>>,
) -> Option<Rc<dyn Fn(RGBA) -> View>> {
    if let Some(attention) = attention {
        let theme = theme.clone();
        return Some(Rc::new(move |color| {
            let alert = rgba(theme().text.feedback.warning.base);
            let el = text();
            el.set_attributes(Some(text_attributes::BOLD));
            el.set_fg(Some(if color == rgba(theme().text.action.primary.focused) { color } else { alert }));
            insert_node(el, text_node_from_string(if attention == "permission" { "!" } else { "?" }), None);
            View::Node(el)
        }));
    }
    if running {
        return Some(Rc::new(move |color| {
            spinner(SpinnerProps { color: Some(Rc::new(move || Some(color))), ..SpinnerProps::default() })
        }));
    }
    fallback
}

/// `<DialogSessionList>`.
pub fn dialog_session_list() -> View {
    let dialog = use_dialog();
    let route = tui().route;
    let data = tui().data.clone();
    let toast = tui().toast.clone();
    let local = crate::local::local();
    let tabs = crate::tabs::tabs();
    let tabs_enabled = tabs.enabled();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let shortcuts = use_shortcuts();
    let (filter, set_filter) = create_signal(String::new());
    let (search, set_search) = create_signal(String::new());
    let search_timer: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
    let (to_delete, set_to_delete) = create_signal::<Option<String>>(None);
    let all_projects = tui().session_list_all_projects;

    // `createDebouncedSignal("", 150)`.
    let schedule_search = {
        let search_timer = search_timer.clone();
        Rc::new(move |value: String| {
            if let Some(timer) = search_timer.take() {
                renderer::clear_timeout(timer);
            }
            let token = renderer::set_timeout(150, move || set_search.set(value));
            search_timer.set(Some(token));
        })
    };
    on_cleanup({
        let search_timer = search_timer.clone();
        move || {
            if let Some(timer) = search_timer.take() {
                renderer::clear_timeout(timer);
            }
        }
    });

    let picker_location = {
        let route = route;
        let data = data.clone();
        let store = data.store.clone();
        move || -> Json {
            if let crate::ctx::Route::Session { session_id } = route.get() {
                if let Some(location) = store.session.get(&session_id).and_then(|s| get(&s, "location").cloned()) {
                    return location;
                }
            }
            store.location.default()
        }
    };
    let (current_project_name, set_current_project_name) = create_signal(String::new());
    {
        let data = data.clone();
        let picker_location = picker_location.clone();
        create_effect(move || {
            let current = data.store.location.info(Some(&picker_location()));
            let name = current
                .and_then(|location| get(&location, "project").and_then(|project| get_str(project, "id")).map(str::to_string))
                .and_then(|id| data.store.project.get(&id))
                .and_then(|project| project_name(Some(&project), ""));
            set_current_project_name.set(name.unwrap_or_default());
        });
    }

    #[derive(Clone, PartialEq)]
    struct SearchResult {
        query: String,
        all_projects: bool,
        sessions: Vec<Json>,
        error: Option<String>,
    }
    let (search_result, set_search_result) = create_signal::<Option<SearchResult>>(None);
    let (search_loading, set_search_loading) = create_signal(false);
    let request_token: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    {
        let data = data.clone();
        let picker_location = picker_location.clone();
        let request_token = request_token.clone();
        create_effect(on(
            move || (search.get(), all_projects.get(), picker_location()),
            move |(query, all_projects, location), _| {
                let data = data.clone();
                let request_token = request_token.clone();
                let query = query.clone();
                let all_projects = *all_projects;
                let location = location.clone();
                let token = request_token.get() + 1;
                request_token.set(token);
                set_search_loading.set(true);
                tokio::task::spawn_local(async move {
                    if data.store.location.info(Some(&location)).is_none() {
                        let _ = data.store.location.sync(Some(&location)).wait().await;
                    }
                    let outcome = match data.store.location.info(Some(&location)) {
                        None => Err("Location unavailable".to_string()),
                        Some(current) => {
                            let mut input = opencode_protocol::wire::Object::new();
                            if !all_projects {
                                let project = get(&current, "project");
                                let project_id = project.and_then(|p| get_str(p, "id")).unwrap_or("global");
                                if project_id == "global" {
                                    if let Some(directory) = get_str(&current, "directory") {
                                        input.insert("directory".into(), Json::String(directory.into()));
                                    }
                                } else {
                                    input.insert("project".into(), Json::String(project_id.into()));
                                    let project_directory =
                                        project.and_then(|p| get_str(p, "directory")).unwrap_or_default();
                                    let directory = get_str(&current, "directory").unwrap_or_default();
                                    input.insert(
                                        "subpath".into(),
                                        Json::String(crate::tools::path_format::relative(project_directory, directory).replace('\\', "/")),
                                    );
                                }
                            }
                            if !query.is_empty() {
                                input.insert("search".into(), Json::String(query.clone()));
                            }
                            input.insert("limit".into(), Json::Number(50.0));
                            input.insert("order".into(), Json::String("desc".into()));
                            input.insert("parentID".into(), Json::Null);
                            input.insert("archived".into(), Json::Bool(false));
                            data.client
                                .call_named("session.list", &Json::Object(input), Default::default())
                                .await
                                .map(|value| array(&value.json()).to_vec())
                                .map_err(|error| error.message())
                        }
                    };
                    if request_token.get() != token {
                        return;
                    }
                    set_search_loading.set(false);
                    set_search_result.set(Some(match outcome {
                        Ok(sessions) => SearchResult { query: query.clone(), all_projects, sessions, error: None },
                        Err(error) => SearchResult { query: query.clone(), all_projects, sessions: Vec::new(), error: Some(error) },
                    }));
                });
            },
            false,
        ));
    }

    let local_sessions = {
        let data = data.clone();
        let picker_location = picker_location.clone();
        create_memo(move || {
            let query = filter.get().trim().to_lowercase();
            let current = data.store.location.info(Some(&picker_location()));
            let all = all_projects.get();
            data.store
                .session
                .list()
                .into_iter()
                .filter(|session| get(session, "time").and_then(|time| get(time, "archived")).is_none())
                .filter(|session| {
                    if all {
                        return true;
                    }
                    let Some(current) = &current else { return false };
                    let project = get(current, "project");
                    let project_id = project.and_then(|p| get_str(p, "id"));
                    let directory = get_str(current, "directory");
                    get_str(session, "projectID") == project_id
                        && get(session, "location").and_then(|l| get_str(l, "directory")) == directory
                })
                .filter(|session| {
                    if query.is_empty() {
                        return true;
                    }
                    let parent = get_str(session, "parentID");
                    parent.is_none_or(|p| p.is_empty())
                        && with_timestamped_fallback(session).to_lowercase().contains(&query)
                })
                .map(|session| (*session).clone())
                .collect::<Vec<_>>()
        })
    };

    let sessions = {
        let local_sessions = local_sessions.clone();
        create_memo(move || {
            let query = filter.get().trim().to_string();
            let local = local_sessions.get();
            let latest = search_result.get();
            if query != search.get().trim() {
                return latest.map(|result| result.sessions).unwrap_or(local);
            }
            if search_loading.get() {
                return latest.map(|result| result.sessions).unwrap_or_default();
            }
            let Some(result) = latest else { return local };
            if result.query != query || result.all_projects != all_projects.get() || result.error.is_some() {
                return local;
            }
            result.sessions
        })
    };
    let search_state = create_memo(move || {
        let query = filter.get().trim().to_string();
        if query != search.get().trim() || search_loading.get() {
            return (if query.is_empty() { "Loading sessions…" } else { "Searching sessions…" }.to_string(), false);
        }
        if let Some(result) = search_result.get() {
            if result.query == query && result.error.is_some() {
                return (
                    if query.is_empty() {
                        "Could not load sessions."
                    } else {
                        "Could not search sessions. Change the search to try again."
                    }
                    .to_string(),
                    true,
                );
            }
        }
        (if query.is_empty() { "No sessions available" } else { "No sessions found" }.to_string(), false)
    });
    let quick_switch_hint = {
        let shortcuts = shortcuts.clone();
        move || -> Option<String> {
            if tabs_enabled {
                return None;
            }
            let first = shortcuts.get("session.quick_switch.1")?;
            let last = shortcuts.get("session.quick_switch.9")?;
            Some(quick_switch_range(&first, &last))
        }
    };

    let options = {
        let data = data.clone();
        let local = local.clone();
        let theme = theme.clone();
        let shortcuts = shortcuts.clone();
        let sessions = sessions.clone();
        create_memo(move || {
            let now = crate::usage_limits::local_time(crate::usage_limits::now_ms());
            let today = now.map(|time| time.to_date_string()).unwrap_or_default();
            let all = sessions.get();
            let session_map: HashMap<String, Json> = all
                .iter()
                .filter(|session| get(session, "parentID").is_none_or(|p| matches!(p, Json::Null)))
                .filter_map(|session| get_str(session, "id").map(|id| (id.to_string(), session.clone())))
                .collect();
            let pinned: Vec<String> = if tabs_enabled {
                Vec::new()
            } else {
                local.pinned().into_iter().filter(|id| session_map.contains_key(id)).collect()
            };
            let pinned_set: HashSet<String> = pinned.iter().cloned().collect();
            let slots: HashMap<String, usize> =
                if tabs_enabled { HashMap::new() } else { local.slots().into_iter().enumerate().map(|(i, id)| (id, i + 1)).collect() };
            let option = |session: &Json, category: &str| -> SelectOption<String> {
                let id = get_str(session, "id").unwrap_or_default().to_string();
                let deleting = to_delete.with(|value| value.as_deref() == Some(id.as_str()));
                let title = if deleting {
                    format!("Press {} again to confirm", shortcuts.get("session.delete").unwrap_or_default())
                } else {
                    with_timestamped_fallback(session)
                };
                let directory = get(session, "location").and_then(|l| get_str(l, "directory")).unwrap_or_default();
                let project = get_str(session, "projectID").and_then(|pid| data.store.project.get(pid));
                let root = match get_str(session, "subpath").filter(|s| !s.is_empty()) {
                    Some(subpath) => {
                        let up = subpath.split('/').map(|_| "..").collect::<Vec<_>>().join("/");
                        crate::tools::path_format::resolve(directory, &up)
                    }
                    None => directory.to_string(),
                };
                let canonical = project.as_ref().and_then(|p| get_str(p, "canonical")).unwrap_or(&root);
                let relative = crate::tools::path_format::relative(canonical, &root);
                let footer = relative_start(&relative).then(|| locale::truncate(&js_basename(&relative), 25));
                let slot = slots.get(&id).copied();
                let running = data.store.session.status(&id) == SessionStatus::Running
                    || data.store.session.family(&id).iter().any(|member| data.store.session.status(member) == SessionStatus::Running);
                let attention = match tabs.status(&id).attention {
                    crate::tabs::Attention::Permission => Some("permission"),
                    crate::tabs::Attention::Question => Some("question"),
                    crate::tabs::Attention::None => None,
                };
                let slot_theme = theme.clone();
                let gutter = session_status_gutter(
                    Rc::new({
                        let theme = theme.clone();
                        move || theme.get()
                    }),
                    attention,
                    running,
                    slot.map(|slot| {
                        let theme = slot_theme.clone();
                        Rc::new(move |_| {
                            let el = text();
                            let color = accent_200(&theme.get());
                            el.set_fg(Some(color));
                            insert_node(el, text_node_from_string(&slot.to_string()), None);
                            View::Node(el)
                        }) as Rc<dyn Fn(RGBA) -> View>
                    }),
                );
                let mut option = SelectOption::new(title, id);
                option.category = Some(category.to_string());
                option.footer = footer.map(crate::ui::dialog_select::Footer::Text);
                if deleting {
                    option.bg = Some(rgba(theme.get().background.action.destructive.focused));
                    option.fg = Some(rgba(theme.get().text.action.destructive.focused));
                }
                option.gutter = gutter;
                option
            };
            let mut out: Vec<SelectOption<String>> = pinned
                .iter()
                .filter_map(|id| session_map.get(id))
                .map(|session| option(session, "Pinned"))
                .collect();
            out.extend(
                sessions
                    .get()
                    .iter()
                    .filter(|session| get(session, "parentID").is_none_or(|p| matches!(p, Json::Null)))
                    .filter(|session| !pinned_set.contains(get_str(session, "id").unwrap_or_default()))
                    .map(|session| {
                        let date = get(session, "time")
                            .and_then(|t| get(t, "updated"))
                            .and_then(|v| match v {
                                Json::Number(n) => Some(*n),
                                _ => None,
                            })
                            .and_then(crate::usage_limits::local_time)
                            .map(|time| time.to_date_string())
                            .unwrap_or_default();
                        option(session, if date == today { "Today" } else { &date })
                    }),
            );
            out
        })
    };

    {
        let dialog = dialog.clone();
        on_mount(move || dialog.set_size(DialogSize::Large));
    }

    let title_view = {
        let theme = theme.clone();
        let current_project_name = current_project_name;
        Rc::new(move || {
            let el = box_();
            el.set_flex_direction(FlexDirection::Row);
            let label = text();
            label.set_attributes(Some(text_attributes::BOLD));
            bind_always(move || rgba(theme.get().text.base), move |c| label.set_fg(Some(c)));
            insert_node(label, text_node_from_string("Sessions"), None);
            insert_node(el, label, None);
            insert(
                el,
                show(
                    move || !all_projects.get() && !current_project_name.get().is_empty(),
                    move || {
                        let suffix = text();
                        bind_always(move || rgba(theme.get().text.muted), move |c| suffix.set_fg(Some(c)));
                        insert(
                            suffix,
                            View::dynamic(move || View::from(format!(" for {}", current_project_name.get()))),
                            At::Only,
                        );
                        View::Node(suffix)
                    },
                    None,
                ),
                At::End,
            );
            View::Node(el)
        }) as Rc<dyn Fn() -> View>
    };
    let title_placeholder = {
        let current_project_name = current_project_name;
        Some(Rc::new(move || {
            if !all_projects.get() && !current_project_name.get().is_empty() {
                format!("Search sessions in {}", current_project_name.get())
            } else {
                "Search sessions".into()
            }
        }) as Rc<dyn Fn() -> String>)
    };
    let empty_view = {
        let theme = theme.clone();
        let search_state = search_state.clone();
        Some(Rc::new(move || search_message_view(&theme, &search_state)) as Rc<dyn Fn() -> View>)
    };
    let no_match_view = {
        let theme = theme.clone();
        let search_state = search_state.clone();
        Some(Rc::new(move || search_message_view(&theme, &search_state)) as Rc<dyn Fn() -> View>)
    };
    let bindings = {
        let all_projects = all_projects;
        Rc::new(move || {
            vec![Command::inline("ctrl+a", move || {
                all_projects.update(|value| *value = !*value);
                true
            })
            .title(if all_projects.get() { "Show current directory sessions" } else { "Show all project sessions" })
            .group("Dialog")]
        }) as Rc<dyn Fn() -> Vec<Command>>
    };
    let actions = {
        let local = local.clone();
        let dialog = dialog.clone();
        let client = data.client.clone();
        let toast = toast.clone();
        Rc::new(move || {
            let mut actions = vec![];
            if !tabs_enabled {
                let local = local.clone();
                actions.push(SelectAction::new("session.pin.toggle", "pin/unpin", move |option: &SelectOption<String>| {
                    local.toggle_pin(&option.value);
                }));
            }
            {
                let client = client.clone();
                let toast = toast.clone();
                actions.push(SelectAction::new("session.delete", "delete", move |option: &SelectOption<String>| {
                    if to_delete.get_untracked().as_deref() != Some(option.value.as_str()) {
                        set_to_delete.set(Some(option.value.clone()));
                        return;
                    }
                    let client = client.clone();
                    let toast = toast.clone();
                    let value = option.value.clone();
                    let input = crate::data::object(vec![("sessionID", Json::String(value.clone()))]);
                    tokio::task::spawn_local(async move {
                        match client.call_named("session.remove", &input, Default::default()).await {
                            Ok(_) => {
                                set_search_result.update(|result| {
                                    if let Some(result) = result {
                                        result.sessions.retain(|session| get_str(session, "id") != Some(value.as_str()));
                                    }
                                });
                            }
                            Err(error) => {
                                set_to_delete.set(None);
                                toast.show(
                                    crate::toast::ToastInput::new(
                                        crate::toast::Variant::Error,
                                        format!("Failed to delete session: {}", error.message()),
                                    )
                                    .duration(5000),
                                );
                            }
                        }
                    });
                }));
            }
            {
                let dialog = dialog.clone();
                actions.push(SelectAction::new("session.rename", "rename", move |option: &SelectOption<String>| {
                    dialog_session_rename::show(&dialog, &option.value, Some(option.title.clone()));
                }));
            }
            actions
        }) as Rc<dyn Fn() -> Vec<SelectAction<String>>>
    };
    let footer_hints = {
        let quick_switch_hint = quick_switch_hint.clone();
        let all_projects = all_projects;
        let local = local.clone();
        let new_designs = crate::ui::dialog::use_new_designs();
        Rc::new(move || {
            let mut hints = vec![];
            if !new_designs() && local.slots().len() > 0 {
                if let Some(hint) = quick_switch_hint() {
                    hints.push(FooterHint { title: "switch".into(), label: hint, side: None, group: None });
                }
            }
            hints.push(FooterHint {
                title: if all_projects.get() { "current directory" } else { "all projects" }.into(),
                label: "ctrl+a".into(),
                side: Some(Side::Right),
                group: None,
            });
            hints
        }) as Rc<dyn Fn() -> Vec<FooterHint>>
    };

    let options_fn = {
        let options = options.clone();
        move || options.get()
    };
    let mut props = SelectProps::new("Sessions", options_fn);
    props.title_view = Some(title_view);
    props.search_in_title = true;
    props.title_placeholder = title_placeholder;
    props.skip_filter = true;
    props.current = Some(Rc::new({
        let route = route;
        move || match route.get() {
            crate::ctx::Route::Session { session_id } => Some(session_id),
            crate::ctx::Route::Home => None,
        }
    }));
    props.focus_current = Rc::new(|| false);
    props.on_filter = Some(Rc::new({
        let schedule_search = schedule_search.clone();
        move |query: &str| {
            set_filter.set(query.to_string());
            schedule_search(query.to_string());
        }
    }));
    props.empty_view = empty_view;
    props.no_match_view = no_match_view;
    props.bindings = bindings;
    props.on_move = Some(Rc::new(move |_: &SelectOption<String>| set_to_delete.set(None)));
    props.on_select = Some(Rc::new({
        let dialog = dialog.clone();
        move |option: &SelectOption<String>| {
            route.set(crate::ctx::Route::Session { session_id: option.value.clone() });
            dialog.clear();
        }
    }));
    props.actions = Some(actions);
    props.footer_hints = footer_hints;
    dialog_select(props)
}

/// The empty and no-match line, error-colored while the search failed.
fn search_message_view(
    theme: &crate::ui::theme::ComponentTheme,
    state: &Memo<(String, bool)>,
) -> View {
    let el = box_();
    el.set_padding(Edge::Left, point(DIALOG_LIST_INSET));
    el.set_padding(Edge::Right, point(DIALOG_LIST_INSET));
    let line = text();
    {
        let theme = theme.clone();
        let state = state.clone();
        bind_always(
            move || {
                let t = theme.get();
                if state.get().1 { rgba(t.text.feedback.error.base) } else { rgba(t.text.muted) }
            },
            move |c| line.set_fg(Some(c)),
        );
    }
    let state = state.clone();
    insert(line, View::dynamic(move || View::from(state.get().0)), At::Only);
    insert_node(el, line, None);
    View::Node(el)
}

/// `quickSwitchRange(first, last)`.
fn quick_switch_range(first: &str, last: &str) -> String {
    let prefix = &first[..first.len().saturating_sub(1)];
    if first.ends_with('1') && last == format!("{prefix}9") {
        return format!("{prefix}1-9");
    }
    format!("{first} through {last}")
}
