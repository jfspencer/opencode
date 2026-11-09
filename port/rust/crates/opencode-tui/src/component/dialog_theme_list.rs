//! `component/dialog-theme-list.tsx`: the themes, previewed as the selection moves and restored unless one is
//! chosen. `on_close` returns to the caller (Settings) instead of closing every dialog.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::on_cleanup;
use opencode_tui_core::solid::View;

use crate::themes::use_themes;
use crate::ui::dialog::use_dialog;
use crate::ui::dialog_select::{SelectOption, SelectProps, SelectRef, dialog_select};

/// The theme names as values of the "Theme" setting.
pub fn theme_names() -> Vec<Json> {
    use_themes().map(|t| t.names()).unwrap_or_default().into_iter().map(Json::String).collect()
}

/// `<DialogThemeList onClose>`.
pub fn dialog_theme_list(on_close: Option<Rc<dyn Fn()>>) -> View {
    let Some(themes) = use_themes() else { return View::Empty };
    let options: Vec<SelectOption<String>> = themes.names().into_iter().map(|name| SelectOption::new(name.clone(), name)).collect();
    let dialog = use_dialog();
    let confirmed = Rc::new(Cell::new(false));
    let select: Rc<RefCell<Option<SelectRef<String>>>> = Rc::new(RefCell::new(None));
    let initial = opencode_tui_core::reactive::untrack(|| themes.selected());
    {
        let themes = themes.clone();
        let confirmed = confirmed.clone();
        let initial = initial.clone();
        on_cleanup(move || {
            if !confirmed.get() {
                themes.set(&initial);
            }
        });
    }
    let mut props = SelectProps::new("Themes", move || options.clone());
    props.search_in_title = true;
    props.title_placeholder = Some("Search themes".into());
    {
        let initial = initial.clone();
        props.current = Some(Rc::new(move || Some(initial.clone())));
    }
    props.on_cancel = on_close.clone();
    {
        let themes = themes.clone();
        props.on_move = Some(Rc::new(move |option: &SelectOption<String>| {
            themes.set(&option.value);
        }));
    }
    {
        let themes = themes.clone();
        let confirmed = confirmed.clone();
        props.on_select = Some(Rc::new(move |option: &SelectOption<String>| {
            themes.set(&option.value);
            confirmed.set(true);
            if let Some(close) = &on_close {
                close();
                return;
            }
            dialog.clear();
        }));
    }
    {
        let select = select.clone();
        props.on_ref = Some(Rc::new(move |r| *select.borrow_mut() = Some(r)));
    }
    props.on_filter = Some(Rc::new(move |query: &str| {
        if query.is_empty() {
            themes.set(&initial);
            return;
        }
        let first = select.borrow().as_ref().and_then(|r| r.filtered().first().map(|o| o.value.clone()));
        if let Some(first) = first {
            themes.set(&first);
        }
    }));
    dialog_select(props)
}
