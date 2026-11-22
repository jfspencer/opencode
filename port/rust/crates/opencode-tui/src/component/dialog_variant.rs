//! `component/dialog-variant.tsx`: the current model's variants, "Default" writing no variant. Under new designs
//! the search field is hidden (the variants are short).
use std::rc::Rc;

use opencode_tui_core::solid::*;

use crate::local::{Local, local};
use crate::ui::dialog::{DialogContext, use_dialog, use_new_designs};
use crate::ui::dialog_select::{SelectOption, SelectProps, dialog_select};

/// `<DialogVariant>`.
pub fn dialog_variant() -> View {
    let local = local();
    let dialog = use_dialog();
    let new_designs = use_new_designs();
    let options = {
        let local = local.clone();
        let dialog = dialog.clone();
        move || variant_options(&local, &dialog)
    };
    let current = {
        let local = local.clone();
        Rc::new(move || {
            Some(
                local
                    .variant_current()
                    .unwrap_or_else(|| "default".to_string()),
            )
        })
    };
    let mut props = SelectProps::new("Select variant", options);
    props.render_filter = !new_designs();
    props.current = Some(current);
    props.flat = true;
    dialog_select(props)
}

fn variant_options(local: &Local, dialog: &DialogContext) -> Vec<SelectOption<String>> {
    let mut options = vec![variant_option(local, dialog, "default", "Default")];
    options.extend(
        local
            .variant_list()
            .into_iter()
            .filter(|variant| variant != "default")
            .map(|variant| {
                let title = variant.clone();
                variant_option(local, dialog, &variant, &title)
            }),
    );
    options
}

fn variant_option(
    local: &Local,
    dialog: &DialogContext,
    value: &str,
    title: &str,
) -> SelectOption<String> {
    let mut option = SelectOption::new(title, value.to_string());
    let local = local.clone();
    let dialog = dialog.clone();
    let selected = if value == "default" {
        None
    } else {
        Some(value.to_string())
    };
    option.on_select = Some(Rc::new(move |_: &DialogContext| {
        dialog.clear();
        local.variant_set(selected.as_deref());
    }));
    option
}
