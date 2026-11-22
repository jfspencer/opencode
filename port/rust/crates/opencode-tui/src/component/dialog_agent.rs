//! `component/dialog-agent.tsx`: the selectable agents (primary and all-mode, not subagents or hidden ones), the
//! current one marked.
use std::rc::Rc;

use opencode_tui_core::reactive::create_memo;
use opencode_tui_core::solid::*;

use crate::local::local;
use crate::ui::dialog::use_dialog;
use crate::ui::dialog_select::{SelectOption, SelectProps, dialog_select};

/// `<DialogAgent>`.
pub fn dialog_agent() -> View {
    let local = local();
    let dialog = use_dialog();
    let options = {
        let local = local.clone();
        create_memo(move || {
            local
                .agent_list()
                .into_iter()
                .map(|agent| {
                    let id = agent.id.clone();
                    let mut option = SelectOption::new(agent.id, id);
                    option.description = agent.description;
                    option
                })
                .collect::<Vec<_>>()
        })
    };
    let current = {
        let local = local.clone();
        Rc::new(move || local.agent_current().map(|agent| agent.id))
    };
    let mut props = SelectProps::new("Select agent", {
        let options = options.clone();
        move || options.get()
    });
    props.search_in_title = true;
    props.title_placeholder = Some("Search agents".to_string());
    props.current = Some(current);
    props.on_select = Some(Rc::new(move |option: &SelectOption<String>| {
        local.agent_set(&option.value);
        dialog.clear();
    }));
    dialog_select(props)
}
