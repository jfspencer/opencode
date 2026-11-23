//! `component/dialog-workspaces.tsx`: the project's checkouts and worktrees, the current marker, moving the session
//! into one, creating one by name (ctrl+a), deleting one (twice to confirm) and refreshing.
//!
//! TODO(port-tui): the session subdirectories (`session.subpath`) that TS appends to the list.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection};
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::obj;

use crate::component::dialog_worktree_name::show_worktree_name;
use crate::ctx::{Route, tui};
use crate::data::{array, get, get_str};
use crate::keymap::use_shortcuts;
use crate::spinner::{SpinnerProps, spinner};
use crate::theme::SurfaceName;
use crate::toast::ToastInput;
use crate::ui::dialog::{DIALOG_LIST_INSET, DialogSize, dialog_width, use_dialog};
use crate::ui::dialog_select::{SelectAction, SelectOption, SelectProps, TruncateTitle, dialog_select, dialog_select_content_width};
use crate::ui::locale;
use crate::ui::theme::use_theme;
use crate::util::{abbreviate_home, rgba};

/// `WorkspaceSelection`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceSelection {
    Directory { directory: String, subdirectory: bool },
    New { name: String },
}

/// `DialogWorkspacesProps`.
#[derive(Clone)]
pub struct DialogWorkspacesProps {
    pub project_id: String,
    pub location: Option<Json>,
    pub current: Option<WorkspaceSelection>,
    pub on_select: Option<Rc<dyn Fn(WorkspaceSelection)>>,
    pub fixture: bool,
}

impl Default for DialogWorkspacesProps {
    fn default() -> Self {
        DialogWorkspacesProps { project_id: String::new(), location: None, current: None, on_select: None, fixture: false }
    }
}

/// `contains(root, directory)`.
fn contains(root: &str, directory: &str) -> bool {
    if root == directory {
        return true;
    }
    let root = root.trim_end_matches('/');
    directory.strip_prefix(root).is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

/// `<DialogWorkspaces>`.
pub fn dialog_workspaces(props: DialogWorkspacesProps) -> View {
    let dialog = use_dialog();
    let data = tui().data.clone();
    let route = tui().route;
    let toast = tui().toast.clone();
    let paths = tui().paths.clone();
    let dimensions = tui().dimensions;
    let theme = use_theme().surface(SurfaceName::Dialog);
    let shortcuts = use_shortcuts();
    let props = Rc::new(props);
    let project_id = props.project_id.clone();
    let (directories, set_directories) = create_signal::<Option<Vec<Json>>>(None);
    let (loading, set_loading) = create_signal(true);
    let (load_error, set_load_error) = create_signal::<Option<String>>(None);
    let (working, set_working) = create_signal(false);
    let (to_delete, set_to_delete) = create_signal::<Option<String>>(None);
    let (removing, set_removing) = create_signal::<Option<String>>(None);
    let show_error = create_memo(move || load_error.get().is_some() && directories.get().is_none());

    {
        let dialog = dialog.clone();
        let fixture = props.fixture;
        let project_id = project_id.clone();
        let client = data.client.clone();
        on_mount(move || {
            dialog.set_size(DialogSize::Xlarge);
            if fixture {
                return;
            }
            let input = obj! { "projectID" => project_id.as_str() };
            let list = client.call_named("worktree.list", &input, RequestOptions::default());
            tokio::task::spawn_local(async move {
                match list.await {
                    Ok(response) => {
                        set_directories.set(Some(array(&response.json()).to_vec()));
                        set_loading.set(false);
                        let input = obj! { "projectID" => project_id.as_str() };
                        let _ = client.call_named("worktree.refresh", &input, RequestOptions::default()).await;
                    }
                    Err(error) => {
                        set_load_error.set(Some(error.message()));
                        set_loading.set(false);
                    }
                }
            });
        });
    }
    {
        let project_id = project_id.clone();
        let client = data.client.clone();
        let listener = data.store.listen(move |event| {
            if event.r#type() != "worktree.updated" || get_str(event.data(), "projectID") != Some(project_id.as_str()) {
                return;
            }
            let input = obj! { "projectID" => project_id.as_str() };
            let client = client.clone();
            tokio::task::spawn_local(async move {
                if let Ok(response) = client.call_named("worktree.list", &input, RequestOptions::default()).await {
                    set_directories.set(Some(array(&response.json()).to_vec()));
                }
            });
        });
        on_cleanup(move || listener.call());
    }

    let location_info = {
        let data = data.clone();
        let props = props.clone();
        create_memo(move || {
            data.store
                .location
                .info(Some(props.location.as_ref().unwrap_or(&data.store.location.default())))
        })
    };
    let canonical = {
        let data = data.clone();
        let project_id = project_id.clone();
        let location_info = location_info.clone();
        create_memo(move || {
            if let Some(info) = location_info.get()
                && get(&info, "project").and_then(|project| get_str(project, "id")) == Some(project_id.as_str())
                && let Some(directory) = get(&info, "project").and_then(|project| get_str(project, "directory"))
            {
                return directory.to_string();
            }
            data.store
                .project
                .get(&project_id)
                .and_then(|project| get_str(&project, "canonical").map(str::to_string))
                .unwrap_or_default()
        })
    };
    let current_directory = {
        let props = props.clone();
        let canonical = canonical.clone();
        create_memo(move || match &props.current {
            Some(WorkspaceSelection::Directory { directory, .. }) => Some(directory.clone()),
            _ => Some(canonical.get()).filter(|canonical| !canonical.is_empty()),
        })
    };
    let current_root = {
        let directories = directories;
        let current_directory = current_directory.clone();
        create_memo(move || {
            let directory = current_directory.get()?;
            let roots = directories.get()?;
            roots
                .iter()
                .filter_map(|root| get_str(root, "directory").map(str::to_string))
                .filter(|root| contains(root, &directory))
                .max_by_key(String::len)
        })
    };

    let options = {
        let paths = paths.clone();
        let theme = theme;
        let dimensions = dimensions;
        let directories = directories;
        let loading = loading;
        let current_root = current_root.clone();
        let to_delete = to_delete;
        let removing = removing;
        let show_error = show_error.clone();
        let shortcuts = shortcuts;
        move || {
            if show_error.get() {
                return Vec::new();
            }
            let data_now = directories.get();
            let current = current_root.get();
            if loading.get() && data_now.is_none() && current.is_none() {
                return Vec::new();
            }
            let mut roots: Vec<Json> = data_now.unwrap_or_default();
            roots.sort_by(|a, b| {
                let a_dir = get_str(a, "directory").unwrap_or_default();
                let b_dir = get_str(b, "directory").unwrap_or_default();
                if Some(a_dir) == current.as_deref() {
                    return std::cmp::Ordering::Less;
                }
                if Some(b_dir) == current.as_deref() {
                    return std::cmp::Ordering::Greater;
                }
                let a_strategy = get_str(a, "strategy").is_some();
                let b_strategy = get_str(b, "strategy").is_some();
                if a_strategy != b_strategy {
                    return if a_strategy { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less };
                }
                if !a_strategy && !b_strategy {
                    return a_dir.len().cmp(&b_dir.len());
                }
                std::cmp::Ordering::Equal
            });
            let title_width =
                dialog_select_content_width(dialog_width(DialogSize::Xlarge).min(dimensions.get().0.saturating_sub(2))).max(1) as usize;
            let theme_value = theme.get();
            roots
                .into_iter()
                .filter_map(|root| {
                    let location = get_str(&root, "directory")?.to_string();
                    let category = if Some(location.as_str()) == current.as_deref() { "Current" } else { "Other" };
                    let title = abbreviate_home(&location, &paths.home);
                    let visible = locale::truncate_left(&title, title_width);
                    let deleting = to_delete.get().as_deref() == Some(location.as_str());
                    let is_removing = removing.get().as_deref() == Some(location.as_str());
                    let display = if is_removing {
                        format!("Deleting {location}")
                    } else if deleting {
                        format!(
                            "Press {} again to confirm",
                            shortcuts.get("dialog.move_session.delete").unwrap_or_else(|| "ctrl+d".into())
                        )
                    } else {
                        visible
                    };
                    let mut option = SelectOption::new(
                        display,
                        WorkspaceSelection::Directory { directory: location.clone(), subdirectory: false },
                    );
                    option.category = Some(category.to_string());
                    option.title_width = Some(title_width);
                    option.truncate_title = TruncateTitle::Left;
                    if deleting {
                        option.bg = Some(rgba(theme_value.background.action.destructive.base));
                    }
                    Some(option)
                })
                .collect()
        }
    };

    let actions = {
        let props = props.clone();
        let toast = toast.clone();
        let dialog = dialog.clone();
        let client = data.client.clone();
        let project_id = project_id.clone();
        let directories = directories;
        let to_delete = to_delete;
        let set_working = set_working;
        let set_removing = set_removing;
        let set_to_delete = set_to_delete;
        Rc::new(move || {
            let mut actions: Vec<SelectAction<WorkspaceSelection>> = Vec::new();
            if matches!(route.get(), Route::Session { .. }) {
                let (client, toast, dialog) = (client.clone(), toast.clone(), dialog.clone());
                actions.push(SelectAction::new("dialog.move_session.move", "move", move |option: &SelectOption<WorkspaceSelection>| {
                    let Route::Session { session_id } = route.get_untracked() else { return };
                    let WorkspaceSelection::Directory { directory, .. } = &option.value else { return };
                    let input = obj! { "sessionID" => session_id.as_str(), "directory" => directory.as_str() };
                    let call = client.call_named("session.move", &input, RequestOptions::default());
                    dialog.clear();
                    let toast = toast.clone();
                    tokio::task::spawn_local(async move {
                        if let Err(error) = call.await {
                            toast.error(error.message());
                        }
                    });
                }));
            }
            {
                let props = props.clone();
                let dialog = dialog.clone();
                actions.push(SelectAction::dialog("dialog.move_session.new", "new", move || {
                    let props = props.clone();
                    let dialog = dialog.clone();
                    tokio::task::spawn_local(async move {
                        let Some(name) = show_worktree_name(&dialog).wait().await else { return };
                        if let Some(on_select) = &props.on_select {
                            on_select(WorkspaceSelection::New { name });
                        }
                    });
                }));
            }
            {
                let (client, toast, project_id) = (client.clone(), toast.clone(), project_id.clone());
                let mut action =
                    SelectAction::new("dialog.move_session.delete", "delete", move |option: &SelectOption<WorkspaceSelection>| {
                        let WorkspaceSelection::Directory { directory, subdirectory } = &option.value else { return };
                        if *subdirectory {
                            return;
                        }
                        if to_delete.get_untracked().as_deref() != Some(directory.as_str()) {
                            set_to_delete.set(Some(directory.clone()));
                            return;
                        }
                        set_working.set(true);
                        set_removing.set(Some(directory.clone()));
                        let input = obj! { "projectID" => project_id.as_str(), "directory" => directory.as_str() };
                        let call = client.call_named("worktree.remove", &input, RequestOptions::default());
                        let toast = toast.clone();
                        let project_id = project_id.clone();
                        let client = client.clone();
                        tokio::task::spawn_local(async move {
                            match call.await {
                                Ok(_) => {
                                    set_removing.set(None);
                                    set_working.set(false);
                                    set_to_delete.set(None);
                                    let input = obj! { "projectID" => project_id.as_str() };
                                    if let Ok(response) = client.call_named("worktree.list", &input, RequestOptions::default()).await {
                                        set_directories.set(Some(array(&response.json()).to_vec()));
                                    }
                                }
                                Err(error) => {
                                    set_removing.set(None);
                                    set_working.set(false);
                                    toast.show(ToastInput::new(crate::toast::Variant::Error, error.message()).title("Failed to delete worktree"));
                                }
                            }
                        });
                    });
                action.disabled = Some(Rc::new(move |option: Option<&SelectOption<WorkspaceSelection>>| {
                    let Some(WorkspaceSelection::Directory { directory, subdirectory }) = option.map(|option| &option.value) else {
                        return true;
                    };
                    if *subdirectory {
                        return true;
                    }
                    let Some(list) = directories.get() else { return true };
                    !list.iter().any(|root| {
                        get_str(root, "directory") == Some(directory.as_str()) && get_str(root, "strategy").is_some()
                    })
                }));
                actions.push(action);
            }
            {
                let (client, toast, project_id) = (client.clone(), toast.clone(), project_id.clone());
                actions.push(SelectAction::dialog("dialog.move_session.refresh", "refresh", move || {
                    let input = obj! { "projectID" => project_id.as_str() };
                    let call = client.call_named("worktree.refresh", &input, RequestOptions::default());
                    let toast = toast.clone();
                    tokio::task::spawn_local(async move {
                        if let Err(error) = call.await {
                            toast.error(error.message());
                        }
                    });
                }));
            }
            actions
        }) as Rc<dyn Fn() -> Vec<SelectAction<WorkspaceSelection>>>
    };

    let title_view = {
        let theme = theme;
        let working = working;
        let loading = loading;
        Rc::new(move || {
            let row = box_();
            row.set_flex_direction(FlexDirection::Row);
            row.set_gap(2, Some(Value::Point(1.0)));
            let label = text();
            label.set_attributes(Some(text_attributes::BOLD));
            label.set_fg(Some(rgba(theme.get().text.base)));
            insert_node(label, text_node_from_string("Worktrees"), None);
            insert_node(row, label, None);
            insert(
                row,
                show(
                    move || working.get() || loading.get(),
                    || spinner(SpinnerProps::default()),
                    None,
                ),
                At::End,
            );
            View::Node(row)
        }) as Rc<dyn Fn() -> View>
    };
    let empty_view = {
        let theme = theme;
        let load_error = load_error;
        let loading = loading;
        let show_error = show_error.clone();
        Some(Rc::new(move || {
            let el = box_();
            el.set_padding(Edge::Left, Some(Value::Point(DIALOG_LIST_INSET)));
            el.set_padding(Edge::Right, Some(Value::Point(DIALOG_LIST_INSET)));
            let line = text();
            insert(
                line,
                View::dynamic(move || {
                    View::from(if show_error.get() {
                        format!(
                            "Could not load worktrees\n{}\nClose and reopen Worktrees to try again.",
                            load_error.get().unwrap_or_default()
                        )
                    } else if loading.get() {
                        "Loading worktrees…".to_string()
                    } else {
                        "No worktrees available".to_string()
                    })
                }),
                At::Only,
            );
            bind_always(
                move || rgba(if show_error.get() { theme.get().text.feedback.error.base } else { theme.get().text.muted }),
                move |c| line.set_fg(Some(c)),
            );
            insert_node(el, line, None);
            View::Node(el)
        }) as Rc<dyn Fn() -> View>)
    };
    let no_match = {
        let theme = theme;
        Some(Rc::new(move || {
            let el = box_();
            el.set_padding(Edge::Left, Some(Value::Point(DIALOG_LIST_INSET)));
            el.set_padding(Edge::Right, Some(Value::Point(DIALOG_LIST_INSET)));
            let line = text();
            line.set_fg(Some(rgba(theme.get().text.muted)));
            insert_node(line, text_node_from_string("No worktrees found"), None);
            insert_node(el, line, None);
            View::Node(el)
        }) as Rc<dyn Fn() -> View>)
    };

    let current = {
        let current_root = current_root.clone();
        Rc::new(move || current_root.get().map(|directory| WorkspaceSelection::Directory { directory, subdirectory: false }))
            as Rc<dyn Fn() -> Option<WorkspaceSelection>>
    };
    let on_select = {
        let props = props.clone();
        Some(Rc::new(move |option: &SelectOption<WorkspaceSelection>| {
            if let Some(on_select) = &props.on_select {
                on_select(option.value.clone());
            }
        }) as Rc<dyn Fn(&SelectOption<WorkspaceSelection>)>)
    };
    let full_height = {
        let dimensions = dimensions;
        move || (dimensions.get().1 as f32 - (dimensions.get().1 / 4) as f32 - 2.0).max(8.0).min(16.0)
    };
    let body = {
        let show_error = show_error.clone();
        let title_view = title_view.clone();
        let empty_view = empty_view.clone();
        let no_match = no_match.clone();
        let current = current.clone();
        let on_select = on_select.clone();
        let actions = actions.clone();
        let to_delete = to_delete;
        let set_to_delete = set_to_delete;
        let loading = loading;
        let removing = removing;
        move || {
            let outer = box_();
            outer.set_min_height(Some(Value::Point(if show_error.get() { 5.0 } else { full_height() })));
            let mut props = SelectProps::new("Worktrees", options.clone());
            props.title_view = Some(title_view.clone());
            props.render_filter = !show_error.get();
            props.flat = true;
            props.empty_view = empty_view.clone();
            props.no_match_view = no_match.clone();
            props.locked = Some(Rc::new(move || show_error.get() || loading.get() || removing.get().is_some()));
            props.current = Some(current.clone());
            props.on_select = on_select.clone();
            props.on_move = Some(Rc::new(move |_: &SelectOption<WorkspaceSelection>| {
                if to_delete.get_untracked().is_some() {
                    set_to_delete.set(None);
                }
            }));
            props.actions = Some(actions.clone());
            insert(outer, component(move || dialog_select(props)), At::Only);
            View::Node(outer)
        }
    };
    body()
}
