//! `component/dialog-stash.tsx`: the stashed prompts, newest first, with the preview, the relative time and the
//! delete-twice action. Selecting one puts it back in the composer.
use std::cell::Cell;
use std::rc::Rc;

use opencode_tui_core::reactive::*;
use opencode_tui_core::solid::*;

use crate::keymap::use_shortcuts;
use crate::prompt::stash::{Stash, StashEntry};
use crate::theme::SurfaceName;
use crate::ui::dialog::use_dialog;
use crate::ui::dialog_select::{SelectAction, SelectOption, SelectProps, dialog_select};
use crate::ui::locale;
use crate::ui::theme::use_theme;
use crate::util::rgba;

/// `getRelativeTime(timestamp)`.
fn relative_time(timestamp: f64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0);
    let seconds = ((now - timestamp) / 1000.0).floor() as i64;
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if seconds < 60 {
        return "just now".into();
    }
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    if hours < 24 {
        return format!("{hours}h ago");
    }
    if days < 7 {
        return format!("{days}d ago");
    }
    locale::datetime(timestamp)
}

/// `getStashPreview(input, maxLength)`.
fn preview(input: &str) -> String {
    let first = input.split('\n').next().unwrap_or_default().trim();
    locale::truncate(first, 50)
}

/// `<DialogStash onSelect>`.
pub fn dialog_stash(stash: Stash, on_select: Rc<dyn Fn(StashEntry)>) -> View {
    let dialog = use_dialog();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let shortcuts = use_shortcuts();
    let to_delete: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
    let (version, set_version) = create_signal(0u32);
    let options = {
        let stash = stash.clone();
        let to_delete = to_delete.clone();
        move || {
            let _ = version.get();
            let entries = stash.list();
            let mut options: Vec<SelectOption<usize>> = entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let deleting = to_delete.get() == Some(index);
                    let line_count = entry.prompt.text.matches('\n').count() + 1;
                    let mut option = SelectOption::new(
                        if deleting {
                            format!(
                                "Press {} again to confirm",
                                shortcuts.get("stash.delete").unwrap_or_else(|| "ctrl+d".into())
                            )
                        } else {
                            preview(&entry.prompt.text)
                        },
                        index,
                    );
                    if deleting {
                        option.bg = Some(rgba(theme.get().background.action.destructive.focused));
                        option.fg = Some(rgba(theme.get().text.action.destructive.focused));
                    }
                    option.description = Some(relative_time(entry.timestamp));
                    if line_count > 1 {
                        option.footer = Some(crate::ui::dialog_select::Footer::Text(format!("~{line_count} lines")));
                    }
                    option
                })
                .collect();
            options.reverse();
            options
        }
    };
    let actions = {
        let stash = stash.clone();
        let to_delete = to_delete.clone();
        Rc::new(move || {
            let stash = stash.clone();
            let to_delete = to_delete.clone();
            vec![SelectAction::new("stash.delete", "delete", move |option: &SelectOption<usize>| {
                if to_delete.get() == Some(option.value) {
                    stash.remove(option.value);
                    to_delete.set(None);
                    set_version.update(|v| *v += 1);
                    return;
                }
                to_delete.set(Some(option.value));
                set_version.update(|v| *v += 1);
            })]
        }) as Rc<dyn Fn() -> Vec<SelectAction<usize>>>
    };
    let mut props = SelectProps::new("Stash", options);
    props.on_move = Some(Rc::new({
        let to_delete = to_delete.clone();
        move |_: &SelectOption<usize>| {
            if to_delete.get().is_some() {
                to_delete.set(None);
                set_version.update(|v| *v += 1);
            }
        }
    }));
    props.on_select = Some(Rc::new({
        let stash = stash.clone();
        let on_select = on_select.clone();
        let dialog = dialog.clone();
        move |option: &SelectOption<usize>| {
            let entries = stash.list();
            if let Some(entry) = entries.get(option.value) {
                stash.remove(option.value);
                on_select(entry.clone());
            }
            dialog.clear();
        }
    }));
    props.actions = Some(actions);
    dialog_select(props)
}
