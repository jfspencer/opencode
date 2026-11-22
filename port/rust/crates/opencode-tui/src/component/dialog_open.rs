//! `component/dialog-open.tsx` (`DialogOpen`, `moveOpenSession`): recent root sessions and the project's
//! worktrees, with the project list after them, the project → worktrees navigation (right, left), the loading
//! footer, the exact-id lookup, and the failure states.
//!
//! TODO(port-tui): the location context (`useLocation`): selecting a project sets the home directory here instead
//! of a separate location ref. Worktree creation (ctrl+n and its prompt) belongs to the worktree slice and is not
//! implemented.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection};
use opencode_tui_sync::data::SessionStatus;
use opencode_tui_sync::event::Event;
use opencode_tui_sync::json::JsonExt;

use crate::component::dialog_session_list::{project_name, session_status_gutter, with_timestamped_fallback};
use crate::ctx::tui;
use crate::data::{array, get, get_str};
use crate::keymap::{Command, use_shortcuts};
use crate::spinner::{SpinnerProps, spinner};
use crate::theme::SurfaceName;
use crate::ui::dialog::{DialogSize, dialog_width, use_dialog};
use crate::ui::dialog_select::{
    SelectOption, SelectProps, SelectRef, dialog_select, dialog_select_content_width,
};
use crate::ui::locale;
use crate::ui::theme::use_theme;
use crate::util::{abbreviate_home, rgba, string_width, truncate_file_path};

const RECENT_LIMIT: usize = 8;
/// `DialogOpenKey`.
pub const DIALOG_OPEN_KEY: &str = "dialog.open";

fn point(v: f32) -> Option<Value> {
    Some(Value::Point(v))
}

fn js_basename(value: &str) -> String {
    std::path::Path::new(value)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn number(value: Option<&Json>) -> Option<f64> {
    match value {
        Some(Json::Number(value)) => Some(*value),
        _ => None,
    }
}

/// `OpenTarget`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    Session { session_id: String },
    Project { directory: String, project_id: Option<String> },
}

/// `OpenView`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenView {
    Projects,
    Worktrees { project_id: String },
}

#[derive(Clone, PartialEq)]
struct OpenSelection {
    view: OpenView,
    filter: String,
    selected: Option<OpenTarget>,
}

/// `<DialogOpen sessions onLoad>`.
pub fn dialog_open() -> View {
    let dialog = use_dialog();
    let route = tui().route;
    let data = tui().data.clone();
    let client = data.client.clone();
    let toast = tui().toast.clone();
    let paths = tui().paths.clone();
    let dimensions = tui().dimensions;
    let theme = use_theme().surface(SurfaceName::Dialog);
    let shortcuts = use_shortcuts();
    let tabs = crate::tabs::tabs();
    let (filter, set_filter) = create_signal(String::new());
    let (selection_moved, set_selection_moved) = create_signal(false);
    let closed = Rc::new(Cell::new(false));
    let (view, _set_view) = create_signal(OpenView::Projects);
    let project_id = {
        let view = view;
        move || match view.get() {
            OpenView::Projects => None,
            OpenView::Worktrees { project_id } => Some(project_id),
        }
    };
    let (focus, _set_focus) = create_signal::<Option<OpenTarget>>(None);
    let select: Rc<RefCell<Option<SelectRef<OpenTarget>>>> = Rc::new(RefCell::new(None));
    let projects_selection: Rc<RefCell<Option<OpenSelection>>> = Rc::new(RefCell::new(None));

    let snapshot: Rc<dyn Fn() -> OpenSelection> = {
        let view = view;
        let select = select.clone();
        Rc::new(move || OpenSelection {
            view: view.get(),
            filter: select.borrow().as_ref().map(SelectRef::filter).unwrap_or_default(),
            selected: select.borrow().as_ref().and_then(|select| select.selected().map(|option| option.value)),
        })
    };
    let restore: Rc<dyn Fn(OpenSelection)> = {
        let view = view;
        let focus = focus;
        let select = select.clone();
        Rc::new(move |next: OpenSelection| {
            batch(|| {
                view.set(next.view.clone());
                set_selection_moved.set(false);
                focus.set(next.selected.clone());
            });
            if let Some(current) = select.borrow().clone() {
                current.set_filter(&next.filter);
                if let Some(selected) = &next.selected {
                    current.move_to(selected);
                }
            }
        })
    };
    let back: Rc<dyn Fn()> = {
        let projects_selection = projects_selection.clone();
        let restore = restore.clone();
        Rc::new(move || {
            if let Some(previous) = projects_selection.borrow().clone() {
                restore(previous);
            }
        })
    };

    // `recent`: the server's latest sessions, kept as the dialog's visible list until the next load.
    let open_sessions = tui().open_sessions;
    let (recent_loading, set_recent_loading) = create_signal(true);
    let (recent_failed, set_recent_failed) = create_signal(false);
    let (projects_loading, set_projects_loading) = create_signal(true);
    let (projects_failed, set_projects_failed) = create_signal(false);
    {
        let client = client.clone();
        let data = data.clone();
        let closed = closed.clone();
        let set_recent_loading = set_recent_loading;
        let set_recent_failed = set_recent_failed;
        on_mount(move || {
            let changed: Rc<RefCell<HashMap<String, Event>>> = Rc::new(RefCell::new(HashMap::new()));
            let listener = {
                let changed = changed.clone();
                data.store.listen(move |event| {
                    if event.r#type() == "session.deleted" || event.r#type() == "session.moved" {
                        if let Some(id) = get_str(event.data(), "sessionID") {
                            changed.borrow_mut().insert(id.to_string(), event.clone());
                        }
                    }
                })
            };
            let listener = Rc::new(Cell::new(Some(listener)));
            let request = client.call_named(
                "session.list",
                &Json::Object(
                    [
                        ("limit".to_string(), Json::Number(50.0)),
                        ("order".to_string(), Json::String("desc".into())),
                        ("parentID".to_string(), Json::Null),
                        ("archived".to_string(), Json::Bool(false)),
                    ]
                    .into_iter()
                    .collect(),
                ),
                Default::default(),
            );
            let client = client.clone();
            let changed = changed.clone();
            let closed = closed.clone();
            let open_sessions = open_sessions;
            let listener = listener.clone();
            tokio::task::spawn_local(async move {
                let result = request.await;
                if let Some(listener) = listener.take() {
                    listener.call();
                }
                let Ok(value) = result else {
                    set_recent_loading.set(false);
                    set_recent_failed.set(true);
                    return;
                };
                let json = value.json();
                let sessions = array(&json);
                if !closed.get() {
                    let previous = open_sessions.get_untracked();
                    let next: Vec<Json> = sessions
                        .iter()
                        .flat_map(|session| {
                            let Some(id) = get_str(session, "id") else { return vec![session.clone()] };
                            let Some(event) = changed.borrow().get(id).cloned() else {
                                return vec![session.clone()];
                            };
                            if event.r#type() == "session.deleted" {
                                return Vec::new();
                            }
                            let base = previous
                                .iter()
                                .find(|entry| get_str(entry, "id") == Some(id))
                                .cloned()
                                .unwrap_or_else(|| session.clone());
                            vec![move_open_session(&base, &event)]
                        })
                        .collect();
                    open_sessions.set(next);
                }
                set_recent_loading.set(false);
                let _ = client;
            });
        });
    }
    {
        let data = data.clone();
        on_mount(move || {
            let request = data.store.project.sync();
            tokio::task::spawn_local(async move {
                match request.wait().await {
                    Ok(()) => set_projects_failed.set(false),
                    Err(_) => set_projects_failed.set(true),
                }
                set_projects_loading.set(false);
            });
        });
    }

    // `matched`: the exact session id in the search box.
    let (matched, set_matched) = create_signal::<Option<Json>>(None);
    let (matched_loading, set_matched_loading) = create_signal(false);
    {
        let client = client.clone();
        create_effect(on(
            move || filter.get().trim().to_string(),
            move |value, _| {
                let exact = regex::Regex::new(r"^ses_[0-9A-Za-z]{26}$").ok().is_some_and(|re| re.is_match(&value));
                if !exact {
                    set_matched.set(None);
                    set_matched_loading.set(false);
                    return;
                }
                set_matched_loading.set(true);
                let client = client.clone();
                let session_id = value.clone();
                tokio::task::spawn_local(async move {
                    let input = crate::data::object(vec![("sessionID", Json::String(session_id.clone()))]);
                    let session = client
                        .call_named("session.get", &input, Default::default())
                        .await
                        .ok()
                        .map(|value| value.json())
                        .filter(|session| get_str(session, "id") == Some(session_id.as_str()));
                    if filter.get_untracked().trim() != session_id {
                        return;
                    }
                    set_matched.set(session);
                    set_matched_loading.set(false);
                });
            },
            false,
        ));
    }

    let sessions = {
        let data = data.clone();
        let open_sessions = open_sessions;
        let matched = matched;
        create_memo(move || {
            let match_id = matched.get().and_then(|session| get_str(&session, "id").map(str::to_string));
            let mut seen: HashSet<String> = HashSet::new();
            let mut list: Vec<Json> = Vec::new();
            for session in data
                .store
                .session
                .list()
                .into_iter()
                .map(|session| (*session).clone())
                .chain(open_sessions.get())
                .chain(matched.get())
            {
                let Some(id) = get_str(&session, "id").map(str::to_string) else { continue };
                let parent = get_str(&session, "parentID").is_some_and(|parent| !parent.is_empty());
                if parent && match_id.as_deref() != Some(id.as_str()) {
                    continue;
                }
                if get(&session, "time").and_then(|time| get(time, "archived")).is_some()
                    && match_id.as_deref() != Some(id.as_str())
                {
                    continue;
                }
                if !seen.insert(id) {
                    continue;
                }
                list.push(session);
            }
            list.sort_by(|a, b| {
                let updated = |session: &Json| {
                    get(session, "time").and_then(|time| get(time, "updated")).and_then(|value| number(Some(value))).unwrap_or(f64::NAN)
                };
                let order = updated(b) - updated(a);
                if order > 0.0 {
                    std::cmp::Ordering::Greater
                } else if order < 0.0 {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            });
            list
        })
    };

    // `worktrees`: the selected project's saved worktrees, with one refresh after each first load.
    let (worktrees, set_worktrees) = create_signal::<Option<Vec<Json>>>(None);
    let (worktrees_loading, set_worktrees_loading) = create_signal(false);
    let (worktree_request, set_worktree_request) = create_signal(0u64);
    let refreshed: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    let worktree_token: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    {
        let client = client.clone();
        let toast = toast.clone();
        let project_id = project_id.clone();
        let refreshed = refreshed.clone();
        let worktree_token = worktree_token.clone();
        create_effect(on(
            move || (project_id(), worktree_request.get()),
            move |(id, _), _| {
                let Some(id) = id.clone() else {
                    set_worktrees.set(None);
                    set_worktrees_loading.set(false);
                    return;
                };
                let token = worktree_token.get() + 1;
                worktree_token.set(token);
                set_worktrees_loading.set(true);
                let client = client.clone();
                let toast = toast.clone();
                let project_id = project_id.clone();
                let refreshed = refreshed.clone();
                let worktree_token = worktree_token.clone();
                let input = crate::data::object(vec![("projectID", Json::String(id.clone()))]);
                tokio::task::spawn_local(async move {
                    let result = client.call_named("worktree.list", &input, Default::default()).await;
                    if worktree_token.get() != token {
                        return;
                    }
                    set_worktrees_loading.set(false);
                    match result {
                        Ok(value) => {
                            let listed = array(&value.json()).to_vec();
                            set_worktrees.set(Some(listed));
                            let first = refreshed.borrow_mut().insert(id.clone());
                            if first && project_id() == Some(id.clone()) {
                                let input = crate::data::object(vec![("projectID", Json::String(id.clone()))]);
                                tokio::task::spawn_local(async move {
                                    let _ = client.call_named("worktree.refresh", &input, Default::default()).await;
                                });
                            }
                        }
                        Err(error) => {
                            toast.show(
                                crate::toast::ToastInput::new(crate::toast::Variant::Error, error.message())
                                    .title("Loading worktrees failed"),
                            );
                            set_worktrees.set(Some(Vec::new()));
                        }
                    }
                });
            },
            false,
        ));
    }
    {
        let data = data.clone();
        let project_id = project_id.clone();
        on_mount(move || {
            let listener = data.store.on("worktree.updated", move |event| {
                if get_str(event.data(), "projectID") == project_id().as_deref() {
                    set_worktree_request.update(|value| *value += 1);
                }
            });
            on_cleanup(move || listener.call());
        });
    }

    let options = {
        let dialog = dialog.clone();
        let paths = paths.clone();
        let data = data.clone();
        let sessions = sessions.clone();
        let dimensions = dimensions;
        create_memo(move || {
            let open_tabs: HashSet<String> =
                if tabs.enabled() { tabs.list().into_iter().map(|tab| tab.session_id).collect() } else { HashSet::new() };
            let current_id = match route.get() {
                crate::ctx::Route::Session { session_id } => Some(data.store.session.root(&session_id)),
                crate::ctx::Route::Home => None,
            };
            let attention_of = |id: &str| match tabs.status(id).attention {
                crate::tabs::Attention::Permission => Some("permission"),
                crate::tabs::Attention::Question => Some("question"),
                crate::tabs::Attention::None => None,
            };
            let candidates: Vec<Json> = sessions
                .get()
                .into_iter()
                .filter(|session| {
                    let Some(id) = get_str(session, "id") else { return false };
                    !open_tabs.contains(id)
                        || (attention_of(id).is_some() && current_id.as_deref() != Some(id))
                })
                .collect();
            let attention_count = candidates
                .iter()
                .filter(|session| get_str(session, "id").is_some_and(|id| attention_of(id).is_some()))
                .count();
            let filter_value = filter.get();
            let query = filter_value.trim();
            let recent = if query.is_empty() {
                candidates[..candidates.len().min(RECENT_LIMIT.max(attention_count))].to_vec()
            } else {
                sessions.get()
            };
            let session_options = recent
                .iter()
                .map(|session| {
                    let id = get_str(session, "id").unwrap_or_default().to_string();
                    let directory = get(session, "location").and_then(|location| get_str(location, "directory")).unwrap_or_default();
                    let project = get_str(session, "projectID").and_then(|project_id| data.store.project.get(project_id));
                    let name = project_name(project.as_deref(), "").unwrap_or_default();
                    let basename = js_basename(directory);
                    let label = if !name.is_empty()
                        && project.as_ref().and_then(|p| get_str(p, "canonical")) != Some(directory)
                        && name.to_lowercase() != basename.to_lowercase()
                    {
                        format!("{name} · {basename}")
                    } else if !name.is_empty() {
                        name
                    } else {
                        basename
                    };
                    let updated = get(session, "time").and_then(|time| get(time, "updated")).and_then(|value| number(Some(value))).unwrap_or(0.0);
                    let footer = format!(
                        "{}{}",
                        if label.is_empty() { String::new() } else { format!("{} · ", locale::truncate(&label, 30)) },
                        time_ago(updated)
                    );
                    let running = data.store.session.status(&id) == SessionStatus::Running
                        || data
                            .store
                            .session
                            .family(&id)
                            .iter()
                            .any(|member| data.store.session.status(member) == SessionStatus::Running);
                    let mut option = SelectOption::new(with_timestamped_fallback(session), OpenTarget::Session { session_id: id.clone() });
                    option.search_text = Some(format!("{id} {directory}"));
                    option.category = Some("Sessions".into());
                    option.footer = Some(crate::ui::dialog_select::Footer::Text(footer));
                    let marker = open_tabs.contains(&id).then(|| {
                        let theme = theme;
                        Rc::new(move |_| {
                            let el = text();
                            el.set_fg(Some(rgba(theme.get().hue.accent[200])));
                            insert_node(el, text_node_from_string("▪"), None);
                            View::Node(el)
                        }) as Rc<dyn Fn(RGBA) -> View>
                    });
                    option.gutter = session_status_gutter(
                        Rc::new({
                            let theme = theme;
                            move || theme.get()
                        }),
                        attention_of(&id),
                        running,
                        marker,
                    );
                    option
                })
                .collect::<Vec<_>>();

            let current = data.store.location.default();
            let current_directory = get_str(&current, "directory").unwrap_or_default().to_string();
            let content_width =
                dialog_select_content_width(dialog_width(dialog.size()).min(dimensions.get().0.saturating_sub(2)));
            let mut seen: HashSet<String> = HashSet::new();
            let project_options = data
                .store
                .project
                .list()
                .into_iter()
                .filter(|project| get_str(project, "canonical").is_some_and(|canonical| canonical != "/"))
                .filter(|project| {
                    get_str(project, "canonical").is_none_or(|canonical| seen.insert(canonical.to_string()))
                })
                .filter_map(|project| {
                    let canonical = get_str(&project, "canonical")?.to_string();
                    let title = project_name(Some(project.as_ref()), "").unwrap_or_else(|| js_basename(&canonical));
                    let footer = abbreviate_home(&canonical, &paths.home);
                    let git = get_str(&project, "vcs") == Some("git");
                    let width = content_width - string_width(&title) as i64 - if git { 2 } else { 0 };
                    let search = format!("{footer} {}", project_name(Some(project.as_ref()), "").unwrap_or_default());
                    Some(SelectOption {
                        title,
                        title_view: None,
                        value: OpenTarget::Project {
                            directory: canonical.clone(),
                            project_id: git.then(|| get_str(&project, "id").unwrap_or_default().to_string()),
                        },
                        description: None,
                        search_text: Some(search),
                        search_footer: None,
                        details: Vec::new(),
                        footer: Some(crate::ui::dialog_select::Footer::Text(format!(
                            "{}{}",
                            truncate_file_path(&footer, width.max(0)),
                            if git { " →" } else { "" }
                        ))),
                        footer_color: None,
                        footer_overhang: None,
                        title_width: None,
                        truncate_title: crate::ui::dialog_select::TruncateTitle::Right,
                        category: Some("Projects".into()),
                        disabled: false,
                        bg: None,
                        fg: None,
                        gutter: (canonical == current_directory).then(|| {
                            let theme = theme;
                            Rc::new(move |_| {
                                let el = text();
                                el.set_fg(Some(rgba(theme.get().text.formfield.selected)));
                                insert_node(el, text_node_from_string("●"), None);
                                View::Node(el)
                            }) as Rc<dyn Fn(RGBA) -> View>
                        }),
                        margin: None,
                        detail: None,
                        on_select: None,
                    })
                })
                .collect::<Vec<_>>();
            session_options.into_iter().chain(project_options).collect::<Vec<_>>()
        })
    };

    let worktree_options = {
        let dialog = dialog.clone();
        let paths = paths.clone();
        let data = data.clone();
        let worktrees = worktrees;
        let worktrees_loading = worktrees_loading;
        let dimensions = dimensions;
        create_memo(move || {
            let Some(id) = project_id() else { return Vec::new() };
            let Some(project) = data.store.project.get(&id) else { return Vec::new() };
            let canonical = get_str(&project, "canonical").unwrap_or_default().to_string();
            let current = data.store.location.default();
            let current_directory = get_str(&current, "directory").unwrap_or_default().to_string();
            let mut directories = vec![canonical.clone()];
            if !worktrees_loading.get() {
                directories.extend(
                    worktrees
                        .get()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|worktree| get_str(worktree, "directory").map(str::to_string)),
                );
            }
            let content_width =
                dialog_select_content_width(dialog_width(dialog.size()).min(dimensions.get().0.saturating_sub(2)));
            let title_of = |directory: &str| {
                if directory == canonical {
                    project_name(Some(project.as_ref()), "").unwrap_or_else(|| js_basename(directory))
                } else {
                    js_basename(directory)
                }
            };
            let width = directories
                .iter()
                .map(|directory| string_width(&title_of(directory)) as i64)
                .max()
                .unwrap_or(0);
            let width = (content_width - width).max(0);
            let mut seen: HashSet<String> = HashSet::new();
            let mut unique: Vec<String> = directories
                .into_iter()
                .filter(|directory| seen.insert(directory.clone()))
                .collect();
            unique.sort_by(|a, b| {
                if *a == canonical {
                    return std::cmp::Ordering::Less;
                }
                if *b == canonical {
                    return std::cmp::Ordering::Greater;
                }
                if *a == current_directory {
                    return std::cmp::Ordering::Less;
                }
                if *b == current_directory {
                    return std::cmp::Ordering::Greater;
                }
                std::cmp::Ordering::Equal
            });
            unique
                .into_iter()
                .map(|directory| {
                    let title = title_of(&directory);
                    let footer = truncate_file_path(&abbreviate_home(&directory, &paths.home), width);
                    let padding = (width - string_width(&footer) as i64).max(0) as usize;
                    let mut option = SelectOption::new(title, OpenTarget::Project { directory: directory.clone(), project_id: None });
                    option.footer = Some(crate::ui::dialog_select::Footer::Text(format!("{footer}{}", " ".repeat(padding))));
                    option.gutter = (directory == current_directory).then(|| {
                        let theme = theme;
                        Rc::new(move |_| {
                            let el = text();
                            el.set_fg(Some(rgba(theme.get().text.formfield.selected)));
                            insert_node(el, text_node_from_string("●"), None);
                            View::Node(el)
                        }) as Rc<dyn Fn(RGBA) -> View>
                    });
                    option
                })
                .collect()
        })
    };

    let title = {
        let data = data.clone();
        Rc::new(move || match project_id() {
            Some(id) => format!(
                "{} / Worktrees",
                project_name(data.store.project.get(&id).as_deref(), "").unwrap_or_else(|| "Project".into())
            ),
            None => "Open".into(),
        }) as Rc<dyn Fn() -> String>
    };
    let placeholder = Some(Rc::new(move || {
        if project_id().is_some() { "Search worktrees…".to_string() } else { "Search sessions and projects…".to_string() }
    }) as Rc<dyn Fn() -> String>);
    let current = {
        let data = data.clone();
        Rc::new(move || match route.get() {
            crate::ctx::Route::Session { session_id } => Some(OpenTarget::Session { session_id: data.store.session.root(&session_id) }),
            crate::ctx::Route::Home => None,
        }) as Rc<dyn Fn() -> Option<OpenTarget>>
    };
    let empty_view = {
        let theme = theme;
        let recent_loading = recent_loading;
        let projects_loading = projects_loading;
        Some(Rc::new(move || {
            show(
                move || !recent_loading.get() && !projects_loading.get(),
                move || {
                    let el = box_();
                    el.set_padding(Edge::Left, point(crate::ui::dialog::DIALOG_LIST_INSET));
                    el.set_padding(Edge::Right, point(crate::ui::dialog::DIALOG_LIST_INSET));
                    let line = text();
                    line.set_fg(Some(rgba(theme.get().text.muted)));
                    insert_node(line, text_node_from_string("No recent sessions or projects"), None);
                    insert_node(el, line, None);
                    View::Node(el)
                },
                None,
            )
        }) as Rc<dyn Fn() -> View>)
    };
    let no_match_view = {
        let theme = theme;
        let recent_loading = recent_loading;
        let projects_loading = projects_loading;
        let matched_loading = matched_loading;
        let shortcuts = shortcuts.clone();
        Some(Rc::new(move || {
            let el = box_();
            el.set_padding(Edge::Left, point(crate::ui::dialog::DIALOG_LIST_INSET));
            el.set_padding(Edge::Right, point(crate::ui::dialog::DIALOG_LIST_INSET));
            let line = text();
            line.set_fg(Some(rgba(theme.get().text.muted)));
            let message = move || {
                if project_id().is_some() {
                    if worktrees_loading.get() {
                        return "Loading worktrees…".to_string();
                    }
                    return "No matching worktrees".to_string();
                }
                if recent_loading.get() || projects_loading.get() || matched_loading.get() {
                    return "Searching sessions and projects…".to_string();
                }
                match shortcuts.get("session.list") {
                    Some(key) => format!("No matches · search all sessions with {key}"),
                    None => "No matches".to_string(),
                }
            };
            insert(line, View::dynamic(move || View::from(message())), At::Only);
            insert_node(el, line, None);
            View::Node(el)
        }) as Rc<dyn Fn() -> View>)
    };
    let footer = {
        let theme = theme;
        let recent_loading = recent_loading;
        let projects_loading = projects_loading;
        let recent_failed = recent_failed;
        let projects_failed = projects_failed;
        let worktrees_loading = worktrees_loading;
        Some(Rc::new(move || {
            // `<Show>` around the whole footer: hidden, it contributes no box (and so no footer gap).
            let visible = move || {
                if project_id().is_some() {
                    return worktrees_loading.get();
                }
                recent_loading.get() || projects_loading.get() || recent_failed.get() || projects_failed.get()
            };
            show(visible, move || {
            let el = box_();
            let rows = box_();
            rows.set_flex_direction(FlexDirection::Column);
            insert_node(el, rows, None);
            let muted = theme;
            let worktree_spinner = {
                let muted = muted;
                move || {
                    spinner(SpinnerProps {
                        color: Some(Rc::new(move || Some(rgba(muted.get().text.muted)))),
                        children: Some(Rc::new(|| View::from("Loading worktrees…"))),
                        ..SpinnerProps::default()
                    })
                }
            };
            insert(
                rows,
                show(
                    move || project_id().is_some() && worktrees_loading.get(),
                    worktree_spinner,
                    None,
                ),
                At::End,
            );
            let refresh_spinner = {
                let muted = muted;
                move || {
                    spinner(SpinnerProps {
                        color: Some(Rc::new(move || Some(rgba(muted.get().text.muted)))),
                        children: Some(Rc::new(|| View::from("Refreshing sessions and projects…"))),
                        ..SpinnerProps::default()
                    })
                }
            };
            insert(
                rows,
                show(
                    move || project_id().is_none() && (recent_loading.get() || projects_loading.get()),
                    refresh_spinner,
                    None,
                ),
                At::End,
            );
            let failure = {
                let theme = theme;
                move || {
                    let line = text();
                    line.set_fg(Some(rgba(theme.get().text.feedback.error.base)));
                    line.set_wrap_mode(WrapMode::Word);
                    insert(
                        line,
                        View::dynamic(move || {
                            let what = match (recent_failed.get(), projects_failed.get()) {
                                (true, true) => "sessions and projects",
                                (true, false) => "sessions",
                                _ => "projects",
                            };
                            View::from(format!("Could not refresh {what}."))
                        }),
                        At::Only,
                    );
                    View::Node(line)
                }
            };
            insert(
                rows,
                show(move || project_id().is_none() && (recent_failed.get() || projects_failed.get()), failure, None),
                At::End,
            );
            View::Node(el)
            }, None)
        }) as Rc<dyn Fn() -> View>)
    };
    let bindings = {
        let select = select.clone();
        let projects_selection = projects_selection.clone();
        let restore = restore.clone();
        let back = back.clone();
        let snapshot = snapshot.clone();
        Rc::new(move || {
            if project_id().is_none() {
                let select = select.clone();
                let projects_selection = projects_selection.clone();
                let restore = restore.clone();
                let snapshot = snapshot.clone();
                return vec![Command::inline("right", move || {
                    let target = select.borrow().as_ref().and_then(|select| select.selected()).map(|option| option.value);
                    let Some(OpenTarget::Project { directory, project_id: Some(project_id) }) = target else { return true };
                    let snapshot = snapshot();
                    *projects_selection.borrow_mut() = Some(snapshot);
                    restore(OpenSelection {
                        view: OpenView::Worktrees { project_id },
                        filter: String::new(),
                        selected: Some(OpenTarget::Project { directory, project_id: None }),
                    });
                    true
                })
                .title("Show project worktrees")
                .group("Dialog")];
            }
            let back = back.clone();
            vec![
                Command::inline("left", move || {
                    back();
                    true
                })
                .title("Return to projects")
                .group("Dialog"),
                // TODO(port-tui): worktree creation (the worktree slice).
                Command::inline("ctrl+n", || true).title("New worktree").group("Dialog"),
            ]
        }) as Rc<dyn Fn() -> Vec<Command>>
    };
    let footer_hints = Rc::new(move || {
        if project_id().is_some() {
            vec![crate::ui::dialog_select::FooterHint {
                title: "new worktree".into(),
                label: "ctrl+n".into(),
                side: None,
                group: None,
            }]
        } else {
            Vec::new()
        }
    }) as Rc<dyn Fn() -> Vec<crate::ui::dialog_select::FooterHint>>;

    let options_fn = {
        let options = options.clone();
        let worktree_options = worktree_options.clone();
        move || {
            if project_id().is_some() { worktree_options.get() } else { options.get() }
        }
    };
    let mut props = SelectProps::new("Open", options_fn);
    props.title = title;
    props.placeholder = placeholder;
    props.current = Some(current);
    props.focus_target = Some(Rc::new(move || focus.get()));
    props.focus_current = Rc::new(move || focus.get().is_some());
    props.preserve_selection = Rc::new(move || selection_moved.get());
    props.section_navigation = true;
    props.on_move = Some(Rc::new(move |_: &SelectOption<OpenTarget>| set_selection_moved.set(true)));
    props.on_cancel = Some(Rc::new({
        let back = back.clone();
        let dialog = dialog.clone();
        move || {
            if project_id().is_some() {
                back();
            } else {
                dialog.clear();
            }
        }
    }));
    props.on_filter = Some(Rc::new(move |query: &str| set_filter.set(query.to_string())));
    props.empty_view = empty_view;
    props.no_match_view = no_match_view;
    props.footer = footer;
    props.bindings = bindings;
    props.footer_hints = footer_hints;
    {
        let dialog = dialog.clone();
        let select = select.clone();
        let restore = restore.clone();
        props.on_ref = Some(Rc::new(move |value: SelectRef<OpenTarget>| {
            *select.borrow_mut() = Some(value);
            dialog.set_size(DialogSize::Large);
            let pending = Option::<OpenSelection>::None;
            if let Some(previous) = pending {
                restore(previous);
            }
        }));
    }
    props.on_select = Some(Rc::new({
        let dialog = dialog.clone();
        let data = data.clone();
        move |option: &SelectOption<OpenTarget>| {
            dialog.clear();
            match &option.value {
                OpenTarget::Session { session_id } => route.set(crate::ctx::Route::Session { session_id: session_id.clone() }),
                OpenTarget::Project { directory, .. } => {
                    route.set(crate::ctx::Route::Home);
                    // TODO(port-tui): the location slice's `location.set`; the home route reads this directory.
                    data.directory.set(Some(directory.clone()));
                }
            }
        }
    }));
    let _ = projects_failed;
    dialog_select(props)
}

/// `moveOpenSession(session, event)`.
pub fn move_open_session(session: &Json, event: &Event) -> Json {
    let mut next = session.clone();
    let data = event.data();
    if let Some(location) = get(data, "location") {
        next.set("location", location.clone());
    }
    if let Some(project_id) = get(data, "projectID").filter(|value| !matches!(value, Json::Null)) {
        next.set("projectID", project_id.clone());
    }
    if let Some(subpath) = get(data, "subpath") {
        next.set("subpath", subpath.clone());
    }
    let previous = get(session, "time").and_then(|time| get(time, "updated")).and_then(|value| number(Some(value))).unwrap_or(0.0);
    let updated = event.created().map(|created| previous.max(created)).unwrap_or(previous);
    let time = get(session, "time").cloned().unwrap_or(Json::Object(Default::default()));
    let Json::Object(mut time) = time else { return next };
    time.insert("updated".into(), Json::Number(updated));
    next.set("time", Json::Object(time));
    next
}

/// `timeAgo(timestamp)`.
fn time_ago(timestamp: f64) -> String {
    let minutes = ((crate::usage_limits::now_ms() - timestamp) / 60_000.0).floor();
    if minutes < 1.0 {
        return "now".into();
    }
    if minutes < 60.0 {
        return format!("{}m", minutes as i64);
    }
    let hours = (minutes / 60.0).floor();
    if hours < 24.0 {
        return format!("{}h", hours as i64);
    }
    let days = (hours / 24.0).floor();
    if days < 30.0 {
        return format!("{}d", days as i64);
    }
    let months = (days / 30.0).floor();
    if months < 12.0 {
        return format!("{}mo", months as i64);
    }
    format!("{}y", (days / 365.0).floor() as i64)
}
