//! `component/dialog-model.tsx` and `component/dialog-model-free.tsx`: the model picker. Favorites and recent
//! models get their own sections above the provider-grouped list; typing searches the models themselves (title and
//! category) and names each result's provider inline. Choosing a model opens the variant picker when the model has
//! variants and the current variant is not one of them.
//!
//! TODO(port-tui): the usage limits action (`model.dialog.usage`) and the integration dialog the connect action
//! opens; both are separate slices.
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use opencode_tui_core::reactive::*;
use opencode_tui_core::solid::*;

use crate::component::dialog_variant::dialog_variant;
use crate::component::model_options::{
    Ranked, favorite_footer, is_free_model, model_detail, prioritize_favorites, sort_model_options,
    value_key,
};
use crate::ctx::tui;
use crate::data::{ModelInfo, ProviderInfo};
use crate::fuzzysort;
use crate::local::{Local, local};
use crate::model_preference::{ModelRef, model_preference_key};
use crate::ui::dialog::use_dialog;
use crate::ui::dialog_select::{
    Detail, Footer, SelectAction, SelectOption, SelectProps, dialog_select,
};

/// `DialogModelProps`: `providerID`.
#[derive(Clone, Default)]
pub struct DialogModelProps {
    pub provider_id: Option<String>,
}

/// `<DialogModel providerID>`.
pub fn dialog_model(props: DialogModelProps) -> View {
    let local = local();
    let dialog = use_dialog();
    let new_designs = crate::ui::dialog::use_new_designs();
    let connected = tui().data.connected;
    let provider_id = props.provider_id.clone();
    let select = {
        let local = local.clone();
        let dialog = dialog.clone();
        Rc::new(move |provider_id: String, model_id: String| {
            select_model(&local, &dialog, &provider_id, &model_id)
        })
    };
    // TODO(port-tui): `dialog.replace(() => <DialogIntegration onConnected={...} />)`.
    let connect: Rc<dyn Fn()> = Rc::new(|| {});
    let condition = {
        let provider_id = provider_id.clone();
        move || new_designs() && !connected.get() && provider_id.is_none()
    };
    let free = {
        let select = select.clone();
        let connect = connect.clone();
        move || {
            let select = select.clone();
            let connect = connect.clone();
            component(move || dialog_free_models(select, connect))
        }
    };
    let connected_models = {
        let select = select.clone();
        let connect = connect.clone();
        let provider_id = provider_id.clone();
        move || {
            let select = select.clone();
            let connect = connect.clone();
            let props = DialogModelProps {
                provider_id: provider_id.clone(),
            };
            component(move || dialog_connected_models(props, select, connect))
        }
    };
    show(condition, free, Some(Rc::new(connected_models)))
}

/// `select(providerID, modelID)`.
fn select_model(
    local: &Local,
    dialog: &crate::ui::dialog::DialogContext,
    provider_id: &str,
    model_id: &str,
) {
    local.model_set(&ModelRef::new(provider_id, model_id), true);
    let list = local.variant_list();
    let current = local.variant_current();
    if current
        .as_deref()
        .is_some_and(|current| list.iter().any(|variant| variant == current))
    {
        dialog.clear();
        return;
    }
    if !list.is_empty() {
        dialog.replace(|| component(dialog_variant), None, None, None);
        return;
    }
    dialog.clear();
}

type OnSelect = Rc<dyn Fn(String, String)>;

/// One option plus what the ordering reads (`sortModelOptions`).
#[derive(Clone)]
struct ModelOption {
    option: SelectOption<ModelRef>,
    provider_id: String,
    provider_name: String,
    free: bool,
}

impl Ranked for ModelOption {
    fn provider_id(&self) -> Option<&str> {
        Some(&self.provider_id)
    }
    fn provider_name(&self) -> Option<&str> {
        Some(&self.provider_name)
    }
    fn free(&self) -> bool {
        self.free
    }
}

fn provider_name(providers: &[ProviderInfo], provider_id: &str) -> String {
    providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .map(|provider| provider.name.clone())
        .unwrap_or_else(|| provider_id.to_string())
}

fn trailing(model: &ModelInfo, favorite: bool, new_designs: bool) -> Option<Footer> {
    if new_designs {
        return favorite.then(favorite_footer);
    }
    if is_free_model(model) {
        return Some(Footer::Text("Free".to_string()));
    }
    None
}

/// `newDesigns() ? () => <ModelDetail model={model} usage={usage(model)} /> : undefined` (the usage limits of the
/// console slice are absent, so `usage` is undefined).
fn detail(new_designs: bool, model: &ModelInfo) -> Option<Detail> {
    if !new_designs {
        return None;
    }
    let model = model.clone();
    Some(Detail::View(Rc::new(move || {
        model_detail(model.clone(), None, None)
    })))
}

fn is_favorite_of(local: &Local, value: &ModelRef) -> bool {
    local
        .favorite()
        .iter()
        .any(|item| item.provider_id == value.provider_id && item.model_id == value.model_id)
}

/// A model's option for the picker's sections and groups.
fn option_for(
    model: &ModelInfo,
    providers: &[ProviderInfo],
    new_designs: bool,
    favorite: bool,
    category: Option<String>,
    description: Option<String>,
    on_select: &OnSelect,
) -> ModelOption {
    let value = ModelRef::new(model.provider_id.clone(), model.id.clone());
    let mut option = SelectOption::new(model.name.clone(), value.clone());
    option.description = description;
    option.category = category;
    option.footer = trailing(model, favorite, new_designs);
    option.detail = detail(new_designs, model);
    let selected = model.clone();
    let on_select = on_select.clone();
    option.on_select = Some(Rc::new(move |_| {
        on_select(selected.provider_id.clone(), selected.id.clone())
    }));
    let provider_name = provider_name(providers, &value.provider_id);
    ModelOption {
        option,
        provider_id: value.provider_id,
        provider_name,
        free: is_free_model(model),
    }
}

fn to_selected(options: Vec<ModelOption>) -> Vec<SelectOption<ModelRef>> {
    options.into_iter().map(|option| option.option).collect()
}

/// `DialogConnectedModels`: the provider-grouped model list.
fn dialog_connected_models(
    props: DialogModelProps,
    on_select: Rc<dyn Fn(String, String)>,
    on_connect: Rc<dyn Fn()>,
) -> View {
    let local = local();
    let data = tui().data.clone();
    let new_designs = crate::ui::dialog::use_new_designs();
    let connected = data.connected;
    // Under new designs, favoriting does not reorder the list until the dialog is reopened.
    let favorites_at_open = local.favorite();
    let favorite_priority: HashSet<String> =
        favorites_at_open.iter().map(model_preference_key).collect();
    let (query, set_query) = create_signal(String::new());
    let provider_id = props.provider_id.clone();
    let provider_data = data.clone();
    let providers = create_memo(move || provider_data.providers.get().unwrap_or_default());
    let model_data = data.clone();
    let models = create_memo(move || model_data.models.get().unwrap_or_default());
    let show_extra = {
        let provider_id = provider_id.clone();
        move || connected.get() && provider_id.is_none()
    };
    let free_group = {
        let new_designs = new_designs.clone();
        let provider_id = provider_id.clone();
        Rc::new(move |model: &ModelInfo| {
            new_designs()
                && provider_id.is_none()
                && model.provider_id == "opencode"
                && is_free_model(model)
        })
    };

    let options = {
        let local = local.clone();
        let providers = providers.clone();
        let models = models.clone();
        let free_group = free_group.clone();
        let provider_id = provider_id.clone();
        let new_designs = new_designs.clone();
        let favorite_priority = favorite_priority.clone();
        let favorites_at_open = favorites_at_open.clone();
        let on_select = on_select.clone();
        let show_extra = show_extra.clone();
        create_memo(move || {
            let needle = query.get().trim().to_string();
            let show_sections = show_extra() && needle.is_empty();
            let favorites: Vec<ModelRef> = if connected.get() {
                if new_designs() {
                    favorites_at_open.clone()
                } else {
                    local.favorite()
                }
            } else {
                Vec::new()
            };
            let recents = local.recent();
            let models = models.get();
            let providers = providers.get();
            let designs = new_designs();
            let is_connected = connected.get();
            let section = |items: &[ModelRef], category: &str| -> Vec<ModelOption> {
                if !show_sections {
                    return Vec::new();
                }
                items
                    .iter()
                    .filter_map(|item| {
                        let model = models.iter().find(|model| {
                            model.provider_id == item.provider_id && model.id == item.model_id
                        })?;
                        let provider = providers
                            .iter()
                            .find(|provider| provider.id == model.provider_id);
                        Some(option_for(
                            model,
                            &providers,
                            designs,
                            is_favorite_of(&local, item),
                            Some(category.to_string()),
                            Some(
                                provider
                                    .map(|provider| provider.name.clone())
                                    .unwrap_or(model.provider_id.clone()),
                            ),
                            &on_select,
                        ))
                    })
                    .collect()
            };
            let favorite_options = section(&favorites, "Favorites");
            let recent_items: Vec<ModelRef> = recents
                .iter()
                .filter(|item| {
                    !favorites.iter().any(|favorite| {
                        favorite.provider_id == item.provider_id
                            && favorite.model_id == item.model_id
                    })
                })
                .cloned()
                .collect();
            let recent_options = section(&recent_items, "Recent");
            let mut model_options: Vec<ModelOption> = models
                .iter()
                .filter(|model| model.status != "deprecated")
                .filter(|model| {
                    provider_id
                        .as_ref()
                        .is_none_or(|id| model.provider_id == *id)
                })
                .filter(|model| {
                    if !show_sections {
                        return true;
                    }
                    if favorites.iter().any(|item| {
                        item.provider_id == model.provider_id && item.model_id == model.id
                    }) {
                        return false;
                    }
                    !recent_items.iter().any(|item| {
                        item.provider_id == model.provider_id && item.model_id == model.id
                    })
                })
                .map(|model| {
                    let value = ModelRef::new(model.provider_id.clone(), model.id.clone());
                    let favorite = is_favorite_of(&local, &value);
                    let category = if is_connected {
                        Some(if free_group(model) {
                            "Free".to_string()
                        } else {
                            provider_name(&providers, &model.provider_id)
                        })
                    } else {
                        None
                    };
                    let description = if designs {
                        None
                    } else if favorite {
                        Some("(Favorite)".to_string())
                    } else {
                        None
                    };
                    option_for(
                        model,
                        &providers,
                        designs,
                        favorite,
                        category,
                        description,
                        &on_select,
                    )
                })
                .collect();
            model_options = sort_model_options(&model_options, is_connected);
            if !needle.is_empty() {
                let title = |option: &ModelOption| Some(option.option.title.clone());
                let category = |option: &ModelOption| option.option.category.clone();
                let results = fuzzysort::go_keys(
                    &needle,
                    &model_options,
                    &[&title, &category],
                    &fuzzysort::Options {
                        threshold: 0.0,
                        limit: None,
                        score_fn: None,
                    },
                );
                let items = results
                    .iter()
                    .map(|result| model_options[result.index].clone())
                    .collect::<Vec<_>>();
                let sorted = prioritize_favorites(
                    &sort_model_options(&items, false),
                    &favorite_priority,
                    |option| value_key(&option.option.value),
                );
                // Search results name the provider inline. Without a category, the trailing slot keeps the favorite
                // marker instead of falling back to the provider name.
                return sorted
                    .into_iter()
                    .map(|mut option| {
                        if designs {
                            option.option.description = Some(option.provider_name.clone());
                            option.option.category = None;
                        }
                        option.option
                    })
                    .collect();
            }
            if !is_connected {
                return to_selected(prioritize_favorites(
                    &model_options,
                    &favorite_priority,
                    |option| value_key(&option.option.value),
                ));
            }
            let mut all = favorite_options;
            all.extend(recent_options);
            all.extend(
                model_options
                    .iter()
                    .filter(|option| option.option.category.as_deref() != Some("Free"))
                    .cloned(),
            );
            all.extend(
                model_options
                    .iter()
                    .filter(|option| option.option.category.as_deref() == Some("Free"))
                    .cloned(),
            );
            to_selected(all)
        })
    };

    let title = provider_id
        .as_ref()
        .and_then(|id| {
            providers
                .get()
                .into_iter()
                .find(|provider| provider.id == *id)
        })
        .map(|provider| provider.name)
        .unwrap_or_else(|| "Select model".to_string());
    let actions = {
        let new_designs = new_designs.clone();
        let connected = connected;
        let on_connect = on_connect.clone();
        let local = local.clone();
        move || {
            let provider_action = SelectAction::dialog(
                "model.dialog.provider",
                if new_designs() {
                    if connected.get() {
                        "connect integration"
                    } else {
                        "view all integrations"
                    }
                } else if connected.get() {
                    "Connect an integration"
                } else {
                    "View all integrations"
                },
                {
                    let on_connect = on_connect.clone();
                    move || on_connect()
                },
            );
            let mut favorite_action = SelectAction::new("model.dialog.favorite", "Favorite", {
                let local = local.clone();
                move |option: &SelectOption<ModelRef>| local.toggle_favorite(&option.value)
            });
            favorite_action.title = {
                let new_designs = new_designs.clone();
                let local = local.clone();
                Rc::new(move |option: Option<&SelectOption<ModelRef>>| {
                    if !new_designs() {
                        return "Favorite".to_string();
                    }
                    if option.is_some_and(|option| is_favorite_of(&local, &option.value)) {
                        "unfavorite"
                    } else {
                        "favorite"
                    }
                    .to_string()
                })
            };
            vec![provider_action, favorite_action]
        }
    };
    let current = {
        let local = local.clone();
        Rc::new(move || local.model_current())
    };
    let mut props = SelectProps::new(title, {
        let options = options.clone();
        move || options.get()
    });
    props.search_in_title = true;
    props.title_placeholder = Some(std::rc::Rc::new(|| "Search models".to_string()));
    props.flat = true;
    props.skip_filter = true;
    props.current = Some(current);
    props.actions = Some(Rc::new(actions));
    props.on_filter = Some(Rc::new(move |value: &str| set_query.set(value.to_string())));
    dialog_select(props)
}

/// `DialogFreeModels`: the free models, for a new-designs TUI with no connected provider.
fn dialog_free_models(on_select: Rc<dyn Fn(String, String)>, on_connect: Rc<dyn Fn()>) -> View {
    let local = local();
    let data = tui().data.clone();
    let favorite_priority: HashSet<String> =
        local.favorite().iter().map(model_preference_key).collect();
    let recent_rank: HashMap<String, usize> = local
        .recent()
        .iter()
        .enumerate()
        .map(|(index, item)| (model_preference_key(item), index))
        .collect();
    let (query, set_query) = create_signal(String::new());
    let options = {
        let data = data.clone();
        let local = local.clone();
        let favorite_priority = favorite_priority.clone();
        let recent_rank = recent_rank.clone();
        let on_select = on_select.clone();
        create_memo(move || {
            let all: Vec<ModelOption> = data
                .models
                .get()
                .unwrap_or_default()
                .into_iter()
                .filter(|model| model.status != "deprecated" && is_free_model(model))
                .map(|model| {
                    let favorite = is_favorite_of(
                        &local,
                        &ModelRef::new(model.provider_id.clone(), model.id.clone()),
                    );
                    let mut option = option_for(&model, &[], true, favorite, None, None, &on_select);
                    option.option.detail = Some(Detail::View(Rc::new({
                        let model = model.clone();
                        move || model_detail(model.clone(), Some(false), None)
                    })));
                    option
                })
                .collect();
            let needle = query.get().trim().to_string();
            if !needle.is_empty() {
                let title = |option: &ModelOption| Some(option.option.title.clone());
                let results = fuzzysort::go_keys(
                    &needle,
                    &all,
                    &[&title],
                    &fuzzysort::Options {
                        threshold: 0.0,
                        limit: None,
                        score_fn: None,
                    },
                );
                let items = results
                    .iter()
                    .map(|result| all[result.index].clone())
                    .collect::<Vec<_>>();
                return to_selected(prioritize_favorites(&items, &favorite_priority, |option| {
                    value_key(&option.option.value)
                }));
            }
            // Favorites first, then most recently used, then the server's order.
            let mut sorted = all;
            sorted.sort_by_key(|option| {
                recent_rank
                    .get(&model_preference_key(&option.option.value))
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            to_selected(prioritize_favorites(
                &sorted,
                &favorite_priority,
                |option| value_key(&option.option.value),
            ))
        })
    };
    let actions = {
        let on_connect = on_connect.clone();
        let local = local.clone();
        move || {
            let provider_action =
                SelectAction::dialog("model.dialog.provider", "connect provider", {
                    let on_connect = on_connect.clone();
                    move || on_connect()
                });
            let mut favorite_action = SelectAction::new("model.dialog.favorite", "favorite", {
                let local = local.clone();
                move |option: &SelectOption<ModelRef>| local.toggle_favorite(&option.value)
            });
            favorite_action.title = {
                let local = local.clone();
                Rc::new(move |option: Option<&SelectOption<ModelRef>>| {
                    if option.is_some_and(|option| is_favorite_of(&local, &option.value)) {
                        "unfavorite"
                    } else {
                        "favorite"
                    }
                    .to_string()
                })
            };
            vec![provider_action, favorite_action]
        }
    };
    let current = {
        let local = local.clone();
        Rc::new(move || local.model_current())
    };
    let mut props = SelectProps::new("Free models", {
        let options = options.clone();
        move || options.get()
    });
    props.description = Some(Rc::new(|| {
        Some("Connect a provider for access to more models".to_string())
    }));
    props.flat = true;
    props.skip_filter = true;
    props.current = Some(current);
    props.actions = Some(Rc::new(actions));
    props.on_filter = Some(Rc::new(move |value: &str| set_query.set(value.to_string())));
    dialog_select(props)
}
