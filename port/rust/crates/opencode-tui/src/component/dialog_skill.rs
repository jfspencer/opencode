//! `component/dialog-skill.tsx`: the location's skills, name-padded so the descriptions line up, inserted into the
//! prompt as an `@id` mention.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::Edge;

use crate::ctx::tui;
use crate::data::{array, get_str};
use crate::theme::SurfaceName;
use crate::ui::dialog::{DIALOG_LIST_INSET, DialogSize, use_dialog};
use crate::ui::dialog_select::{SelectOption, SelectProps, dialog_select};
use crate::ui::theme::use_theme;
use crate::util::rgba;

/// `<DialogSkill location onSelect>`.
pub fn dialog_skill(location: Option<Json>, on_select: Rc<dyn Fn(String)>) -> View {
    let dialog = use_dialog();
    let data = tui().data.clone();
    let theme = use_theme().surface(SurfaceName::Dialog);
    dialog.set_size(DialogSize::Large);
    let (loading, set_loading) = create_signal(false);
    let (error, set_error) = create_signal::<Option<String>>(None);
    {
        let data = data.clone();
        let location = location.clone();
        on_mount(move || {
            if data.store.location.skill.list(location.as_ref()).is_some() {
                return;
            }
            set_loading.set(true);
            let call = data.store.location.skill.sync(location.as_ref());
            tokio::task::spawn_local(async move {
                match call.wait().await {
                    Ok(()) => {}
                    Err(error) => set_error.set(Some(error.message())),
                }
                set_loading.set(false);
            });
        });
    }
    let options = {
        let data = data.clone();
        let location = location.clone();
        move || {
            if error.get().is_some() || loading.get() {
                return Vec::new();
            }
            let list = data.store.location.skill.list(location.as_ref()).map(|list| array(&list).to_vec()).unwrap_or_default();
            let width = list.iter().filter_map(|skill| get_str(skill, "name").map(str::len)).max().unwrap_or(0);
            list.iter()
                .filter_map(|skill| {
                    let id = get_str(skill, "id")?.to_string();
                    let name = get_str(skill, "name").unwrap_or_default();
                    let mut option = SelectOption::new(format!("{name:<width$}"), id.clone());
                    option.description = get_str(skill, "description").map(|description| {
                        description.split_whitespace().collect::<Vec<_>>().join(" ")
                    });
                    let on_select = on_select.clone();
                    let dialog = dialog.clone();
                    option.on_select = Some(Rc::new(move |_| {
                        on_select(id.clone());
                        dialog.clear();
                    }));
                    Some(option)
                })
                .collect()
        }
    };
    let empty_view = {
        let theme = theme;
        let error = error;
        let loading = loading;
        Some(Rc::new(move || {
            let el = box_();
            el.set_padding(Edge::Left, Some(Value::Point(DIALOG_LIST_INSET)));
            el.set_padding(Edge::Right, Some(Value::Point(DIALOG_LIST_INSET)));
            let line = text();
            insert(
                line,
                View::dynamic(move || {
                    View::from(match (error.get(), loading.get()) {
                        (Some(error), _) => format!("Could not load skills\n{error}\nClose and reopen Skills to try again."),
                        (None, true) => "Loading skills…".to_string(),
                        (None, false) => "No skills available".to_string(),
                    })
                }),
                At::Only,
            );
            bind_always(
                move || {
                    rgba(if error.get().is_some() { theme.get().text.feedback.error.base } else { theme.get().text.muted })
                },
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
            insert_node(line, text_node_from_string("No skills found"), None);
            insert_node(el, line, None);
            View::Node(el)
        }) as Rc<dyn Fn() -> View>)
    };
    let mut props = SelectProps::new("Skills", options);
    props.render_filter = error.get_untracked().is_none() && !loading.get_untracked();
    props.locked = Some(Rc::new(move || error.get().is_some() || loading.get()));
    props.empty_view = empty_view;
    props.no_match_view = no_match;
    dialog_select(props)
}
