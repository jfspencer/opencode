//! `component/command-palette.tsx`: the reachable palette commands (suggested ones first while nothing is
//! typed), and the settings while filtering.
use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_core::solid::*;

use crate::component::dialog_config::{SETTINGS, dialog_config, setting_id};
use crate::keymap::{COMMAND_PALETTE_COMMAND, use_commands, use_shortcuts};
use crate::ui::dialog::DialogContext;
use crate::ui::dialog_select::{Footer, SelectOption, SelectProps, SelectRef, dialog_select};

/// `<CommandPaletteDialog>`.
pub fn command_palette() -> View {
    let commands = use_commands();
    let shortcuts = use_shortcuts();
    let options = opencode_tui_core::reactive::create_memo(move || {
        commands
            .get()
            .iter()
            .filter_map(|command| {
                let id = command.id.clone()?;
                if !command.palette || id == COMMAND_PALETTE_COMMAND {
                    return None;
                }
                let footer = shortcuts.all(&id);
                let mut option = SelectOption::new(command.title.clone().unwrap_or_else(|| id.clone()), id.clone());
                option.description = command.description.clone();
                option.category = command.group.clone();
                option.search_text = Some(
                    [Some(id.clone()), command.description.clone()].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "),
                );
                option.search_footer = Some(Footer::Text(
                    [command.group.clone(), footer.clone()].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
                ));
                option.footer = footer.map(Footer::Text);
                let run = command.clone();
                option.on_select = Some(Rc::new(move |dialog: &DialogContext| {
                    dialog.clear();
                    run.invoke(None);
                }));
                Some((option, command.is_suggested()))
            })
            .collect::<Vec<_>>()
    });
    let setting_options: Vec<SelectOption<String>> = SETTINGS
        .iter()
        .map(|setting| {
            let mut option = SelectOption::new(setting.title, format!("setting:{}", setting_id(setting)));
            option.category = Some(setting.category.to_string());
            option.search_text = (!setting.keywords.is_empty()).then(|| setting.keywords.join(" "));
            option.search_footer = Some(Footer::Text(format!("Settings · {}", setting.category)));
            let id = setting_id(setting);
            option.on_select = Some(Rc::new(move |dialog: &DialogContext| {
                let id = id.clone();
                dialog.show(move || {
                    let id = id.clone();
                    component(move || dialog_config(Some(id)))
                });
            }));
            option
        })
        .collect();
    let select: Rc<RefCell<Option<SelectRef<String>>>> = Rc::new(RefCell::new(None));
    let list = {
        let select = select.clone();
        move || {
            let filtering = select.borrow().as_ref().is_some_and(|r| !r.filter().is_empty());
            let all: Vec<SelectOption<String>> = options.get().into_iter().map(|(o, _)| o).collect();
            if filtering {
                return all.into_iter().chain(setting_options.iter().cloned()).collect();
            }
            let suggested = options.get().into_iter().filter(|(_, s)| *s).map(|(o, _)| {
                let mut option = o;
                option.value = format!("suggested:{}", option.value);
                option.category = Some("Suggested".into());
                option
            });
            suggested.chain(all).collect()
        }
    };
    let mut props = SelectProps::new("Commands", list);
    props.on_ref = Some(Rc::new(move |r| *select.borrow_mut() = Some(r)));
    props.search_in_title = true;
    props.title_placeholder = Some("Search commands".into());
    props.flat = true;
    props.filter_threshold = Some(0.7);
    dialog_select(props)
}
