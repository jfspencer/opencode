//! `component/dialog-integration.tsx`, for the API-key connect flow: the integration list (popular providers first,
//! connection summaries, the connected check mark), the connect method and the key prompt.
//!
//! TODO(port-tui): the OAuth, command and external methods (their attempt dialogs and polling), account rename and
//! the multi-step forms. The list and the API-key method are complete.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::Edge;
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::tui;
use crate::data::{array, get, get_str};
use crate::theme::SurfaceName;
use crate::toast::ToastInput;
use crate::ui::dialog::{DIALOG_LIST_INSET, DialogSize, use_dialog};
use crate::ui::dialog_prompt::{PromptProps, dialog_prompt};
use crate::ui::dialog_select::{SelectAction, SelectOption, SelectProps, dialog_select};
use crate::ui::theme::use_theme;
use crate::util::rgba;

const INTEGRATION_PRIORITY: [(&str, i64); 6] =
    [("opencode", 0), ("opencode-go", 1), ("openai", 2), ("github-copilot", 3), ("anthropic", 4), ("google", 5)];

type OnConnected = Rc<dyn Fn(Option<String>)>;

fn priority(id: &str) -> i64 {
    INTEGRATION_PRIORITY.iter().find(|(name, _)| *name == id).map(|(_, value)| *value).unwrap_or(99)
}

fn connections(integration: &Json) -> Vec<Json> {
    get(integration, "connections").map(|value| array(value).to_vec()).unwrap_or_default()
}

fn credential_connections(integration: &Json) -> Vec<Json> {
    connections(integration).into_iter().filter(|connection| get_str(connection, "type") == Some("credential")).collect()
}

fn methods(integration: &Json) -> Vec<Json> {
    let mut methods: Vec<Json> = get(integration, "methods")
        .map(|value| array(value).to_vec())
        .unwrap_or_default()
        .into_iter()
        .filter(|method| get_str(method, "type") != Some("env"))
        .collect();
    methods.sort_by_key(|method| get_str(method, "type") == Some("key"));
    methods
}

/// `connectionSummary(integration)`.
fn connection_summary(integration: &Json) -> String {
    let connections = connections(integration);
    if connections.len() > 1 {
        return format!("{} connections", connections.len());
    }
    let Some(connection) = connections.first() else { return String::new() };
    if get_str(connection, "type") == Some("credential") {
        return get_str(connection, "label").unwrap_or_default().to_string();
    }
    format!("${}", get_str(connection, "name").unwrap_or_default())
}

/// `locationQuery(location)`.
fn location_query(location: &Json) -> Json {
    obj! { "directory" => location.str("directory").unwrap_or_default() }
}

/// `<DialogIntegration onConnected>`.
pub fn dialog_integration(on_connected: Option<OnConnected>) -> View {
    let dialog = use_dialog();
    let data = tui().data.clone();
    let toast = tui().toast.clone();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let location = data.location.r#ref().unwrap_or_else(|| data.store.location.default());
    let integrations = {
        let data = data.clone();
        let location = location.clone();
        create_memo(move || {
            let mut list: Vec<Json> = data
                .store
                .location
                .integration
                .list(Some(&location))
                .map(|list| array(&list).to_vec())
                .unwrap_or_default()
                .into_iter()
                .filter(|integration| get(integration, "metadata").and_then(|metadata| get_str(metadata, "source")) != Some("mcp"))
                .filter(|integration| {
                    get_str(integration, "id") != Some("opencode-go") || !credential_connections(integration).is_empty()
                })
                .collect();
            list.sort_by(|a, b| {
                let a_id = get_str(a, "id").unwrap_or_default();
                let b_id = get_str(b, "id").unwrap_or_default();
                priority(a_id)
                    .cmp(&priority(b_id))
                    .then_with(|| get_str(a, "name").unwrap_or_default().to_lowercase().cmp(&get_str(b, "name").unwrap_or_default().to_lowercase()))
                    .then_with(|| a_id.cmp(b_id))
            });
            list
        })
    };
    let options = {
        let location = location.clone();
        let on_connected = on_connected.clone();
        move || {
            integrations
                .get()
                .into_iter()
                .filter_map(|integration| {
                    let id = get_str(&integration, "id")?.to_string();
                    let name = get_str(&integration, "name").unwrap_or_default().to_string();
                    let methods = methods(&integration);
                    let credentials = credential_connections(&integration);
                    let mut option = SelectOption::new(name.clone(), id.clone());
                    if methods.is_empty() {
                        option.description = Some("Environment only".into());
                    }
                    if let Some(status) = connections(&integration).first().and_then(|connection| get(connection, "status")) {
                        option.footer = Some(crate::ui::dialog_select::Footer::Text("Sign in required →".into()));
                        if get_str(status, "url").is_none() {
                            option.footer_color = Some(rgba(theme.get().text.feedback.warning.base));
                        }
                    } else if !connection_summary(&integration).is_empty() {
                        option.footer = Some(crate::ui::dialog_select::Footer::Text(connection_summary(&integration)));
                    }
                    option.category = Some(if priority(&id) < 99 { "Popular" } else { "Services" }.into());
                    option.disabled = methods.is_empty() && credentials.is_empty();
                    if !connections(&integration).is_empty()
                        && connections(&integration)[0].get("status").is_none()
                    {
                        option.gutter = Some(Rc::new(move |_| {
                            let el = text();
                            el.set_fg(Some(rgba(theme.get().text.feedback.success.base)));
                            insert_node(el, text_node_from_string("✓"), None);
                            View::Node(el)
                        }));
                    }
                    let dialog = dialog.clone();
                    let location = location.clone();
                    let on_connected = on_connected.clone();
                    let (data, toast) = (data.clone(), toast.clone());
                    option.on_select = Some(Rc::new(move |_| {
                        if !credentials.is_empty() {
                            manage_connections(data.clone(), toast.clone(), &integration, &location, &dialog, on_connected.clone());
                            return;
                        }
                        select_method(data.clone(), toast.clone(), &integration, &methods, &location, &dialog, on_connected.clone());
                    }));
                    Some(option)
                })
                .collect()
        }
    };
    let empty_view = {
        let theme = theme;
        Some(Rc::new(move || {
            let el = box_();
            el.set_padding(Edge::Left, Some(Value::Point(DIALOG_LIST_INSET)));
            el.set_padding(Edge::Right, Some(Value::Point(DIALOG_LIST_INSET)));
            let line = text();
            line.set_fg(Some(rgba(theme.get().text.muted)));
            insert_node(line, text_node_from_string("No integrations available"), None);
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
            insert_node(line, text_node_from_string("No integrations found"), None);
            insert_node(el, line, None);
            View::Node(el)
        }) as Rc<dyn Fn() -> View>)
    };
    let mut props = SelectProps::new("Connect an integration", options);
    props.empty_view = empty_view;
    props.no_match_view = no_match;
    dialog_select(props)
}

/// The connected accounts of one integration (the "Add account" entry and the accounts to activate).
fn manage_connections(
    data: Rc<crate::data::Data>,
    toast: crate::toast::Toasts,
    integration: &Json,
    location: &Json,
    dialog: &crate::ui::dialog::DialogContext,
    on_connected: Option<OnConnected>,
) {
    let theme = use_theme().surface(SurfaceName::Dialog);
    let name = get_str(integration, "name").unwrap_or_default().to_string();
    let integration_id = get_str(integration, "id").unwrap_or_default().to_string();
    let methods = methods(integration);
    let initial = integration.clone();
    let location = location.clone();
    dialog.replace(
        {
            let data = data.clone();
            let toast = toast.clone();
            let name = name.clone();
            let integration_id = integration_id.clone();
            let initial = initial.clone();
            let location = location.clone();
            let on_connected = on_connected.clone();
            let methods = methods.clone();
            move || {
                let data = data.clone();
                let toast = toast.clone();
                let name = name.clone();
                let integration_id = integration_id.clone();
                let initial = initial.clone();
                let location = location.clone();
                let on_connected = on_connected.clone();
                let methods = methods.clone();
                let dialog = use_dialog();
                let theme = theme;
                let (to_delete, set_to_delete) = create_signal::<Option<String>>(None);
                let (version, set_version) = create_signal(0u32);
                let current = {
                    let data = data.clone();
                    let location = location.clone();
                    let integration_id = integration_id.clone();
                    move || {
                        let _ = version.get();
                        data.store
                            .location
                            .integration
                            .list(Some(&location))
                            .and_then(|list| array(&list).iter().find(|item| get_str(item, "id") == Some(integration_id.as_str())).cloned())
                    }
                };
                let toast_options = toast.clone();
                let options = {
                    let current = current.clone();
                    let initial = initial.clone();
                    let data = data.clone();
                    let location = location.clone();
                    let dialog = dialog.clone();
                    let on_connected = on_connected.clone();
                    let methods = methods.clone();
                    let to_delete = to_delete;
                    move || {
                        let (data, toast) = (data.clone(), toast_options.clone());
                        let integration = current().unwrap_or_else(|| initial.clone());
                        let mut options: Vec<SelectOption<String>> = Vec::new();
                        if !methods.is_empty() {
                            let mut option = SelectOption::new("Add account", "add".to_string());
                            let (integration, location, dialog, on_connected, methods) =
                                (integration.clone(), location.clone(), dialog.clone(), on_connected.clone(), methods.clone());
                            let (data_for_add, toast_for_add) = (data.clone(), toast.clone());
                            option.on_select = Some(Rc::new(move |_| {
                                select_method(
                                    data_for_add.clone(),
                                    toast_for_add.clone(),
                                    &integration,
                                    &methods,
                                    &location,
                                    &dialog,
                                    on_connected.clone(),
                                );
                            }));
                            options.push(option);
                        }
                        let mut accounts = credential_connections(&integration);
                        accounts.sort_by(|a, b| {
                            get_str(a, "label").unwrap_or_default().to_lowercase().cmp(&get_str(b, "label").unwrap_or_default().to_lowercase())
                        });
                        for account in accounts {
                            let id = get_str(&account, "id").unwrap_or_default().to_string();
                            let confirming = to_delete.get_untracked().as_deref() == Some(id.as_str());
                            let label = get_str(&account, "label").unwrap_or_default();
                            let mut option = SelectOption::new(
                                if confirming {
                                    format!(
                                        "Press {} again to confirm",
                                        crate::keymap::use_shortcuts().get("dialog.integration.delete").unwrap_or_else(|| "ctrl+d".into())
                                    )
                                } else {
                                    label.to_string()
                                },
                                id.clone(),
                            );
                            option.category = Some("Connected accounts".into());
                            if confirming {
                                option.bg = Some(rgba(theme.get().background.action.destructive.focused));
                                option.fg = Some(rgba(theme.get().text.action.destructive.focused));
                            }
                            if let Some(status) = account.get("status") {
                                option.footer = Some(crate::ui::dialog_select::Footer::Text("Sign in required →".into()));
                                if get_str(status, "url").is_none() {
                                    option.footer_color = Some(rgba(theme.get().text.feedback.warning.base));
                                }
                            }
                            let (client, toast, data, location, dialog, on_connected, methods) = (
                                data.client.clone(),
                                toast_options.clone(),
                                data.clone(),
                                location.clone(),
                                dialog.clone(),
                                on_connected.clone(),
                                methods.clone(),
                            );
                            let integration = integration.clone();
                            let account_status = account.get("status").cloned();
                            option.on_select = Some(Rc::new(move |_| {
                                if account_status.as_ref().is_some_and(|status| get_str(status, "url").is_some()) {
                                    return;
                                }
                                if account_status.is_some() {
                                    select_method(data.clone(), toast.clone(), &integration, &methods, &location, &dialog, on_connected.clone());
                                    return;
                                }
                                let input = obj! { "credentialID" => id.as_str() };
                                let call = client.call_named("credential.activate", &input, RequestOptions::default());
                                let toast = toast.clone();
                                let _ = &data;
                                tokio::task::spawn_local(async move {
                                    if let Err(error) = call.await {
                                        toast.error(error.message());
                                    }
                                });
                            }));
                            options.push(option);
                        }
                        options
                    }
                };
                let actions = {
                    let data = data.clone();
                    let toast = toast.clone();
                    let name = name.clone();
                    let current = current.clone();
                    let dialog = dialog.clone();
                    let to_delete = to_delete;
                    Rc::new(move || {
                        let data = data.clone();
                        let toast = toast.clone();
                        let name = name.clone();
                        let initial = initial.clone();
                        let current = current.clone();
                        let dialog = dialog.clone();
                        let to_delete = to_delete;
                        vec![SelectAction::new(
                            "dialog.integration.delete",
                            "delete",
                            move |option: &SelectOption<String>| {
                                if option.value == "add" {
                                    return;
                                }
                                let credential_id = option.value.clone();
                                if to_delete.get_untracked().as_deref() != Some(credential_id.as_str()) {
                                    set_to_delete.set(Some(credential_id));
                                    set_version.update(|v| *v += 1);
                                    return;
                                }
                                let integration = current().unwrap_or_else(|| initial.clone());
                                let final_connection = credential_connections(&integration).len() == 1;
                                let input = obj! { "credentialID" => credential_id.as_str() };
                                let call = data.client.call_named("credential.remove", &input, RequestOptions::default());
                                let toast = toast.clone();
                                let dialog = dialog.clone();
                                let name = name.clone();
                                tokio::task::spawn_local(async move {
                                    match call.await {
                                        Ok(_) => {
                                            set_to_delete.set(None);
                                            set_version.update(|v| *v += 1);
                                            if final_connection {
                                                toast.show(ToastInput::new(crate::toast::Variant::Success, format!("Disconnected {name}")));
                                                dialog.clear();
                                            }
                                        }
                                        Err(error) => {
                                            set_to_delete.set(Some(credential_id.clone()));
                                            toast.error(error.message());
                                        }
                                    }
                                });
                            },
                        )]
                    }) as Rc<dyn Fn() -> Vec<SelectAction<String>>>
                };
                let mut props = SelectProps::new(name.clone(), options);
                props.actions = Some(actions);
                props.on_move = Some(Rc::new(move |_: &SelectOption<String>| {
                    if to_delete.get_untracked().is_some() {
                        set_to_delete.set(None);
                        set_version.update(|v| *v += 1);
                    }
                }));
                props.current = Some(Rc::new(move || credential_connections(&current().unwrap_or_default()).first().and_then(|account| get_str(account, "id").map(str::to_string))) as Rc<dyn Fn() -> Option<String>>);
                props.focus_current = Rc::new(|| false);
                props.preserve_selection = Rc::new(|| true);
                dialog_select(props)
            }
        },
        None,
        None,
        None,
    );
}

/// `selectMethod(integration, methods, location, dialog, onConnected)`.
fn select_method(
    data: Rc<crate::data::Data>,
    toast: crate::toast::Toasts,
    integration: &Json,
    methods: &[Json],
    location: &Json,
    dialog: &crate::ui::dialog::DialogContext,
    on_connected: Option<OnConnected>,
) {
    if methods.len() == 1 {
        open_method(data, toast, integration, &methods[0], location, dialog, on_connected);
        return;
    }
    let name = get_str(integration, "name").unwrap_or_default().to_string();
    let integration = integration.clone();
    let location = location.clone();
    let dialog_for = dialog.clone();
    let methods = methods.to_vec();
    dialog.replace(
        move || {
            let integration = integration.clone();
            let location = location.clone();
            let dialog_for = dialog_for.clone();
            let on_connected = on_connected.clone();
            let methods = methods.clone();
            let options = methods
                .iter()
                .map(|method| {
                    let label = get_str(method, "label").unwrap_or_else(|| {
                        if get_str(method, "type") == Some("key") { "API key" } else { "Connect" }
                    });
                    let value = if get_str(method, "type") == Some("key") {
                        "key".to_string()
                    } else {
                        get_str(method, "id").unwrap_or_default().to_string()
                    };
                    let method = method.clone();
                    let integration = integration.clone();
                    let location = location.clone();
                    let dialog = dialog_for.clone();
                    let on_connected = on_connected.clone();
                    let mut option = SelectOption::new(label, value);
                    let (data, toast) = (data.clone(), toast.clone());
                    option.on_select = Some(Rc::new(move |_| {
                        open_method(data.clone(), toast.clone(), &integration, &method, &location, &dialog, on_connected.clone());
                    }));
                    option
                })
                .collect::<Vec<_>>();
            let mut props = SelectProps::new(format!("Connect {name}"), move || options.clone());
            props.render_filter = false;
            dialog_select(props)
        },
        None,
        None,
        None,
    );
}

/// `openMethod(integration, method, location, dialog, onConnected)`.
fn open_method(
    data: Rc<crate::data::Data>,
    toast: crate::toast::Toasts,
    integration: &Json,
    method: &Json,
    location: &Json,
    dialog: &crate::ui::dialog::DialogContext,
    on_connected: Option<OnConnected>,
) {
    if get_str(method, "type") == Some("key") {
        begin_key(data, toast, integration, method, location, dialog, on_connected);
        return;
    }
    // TODO(port-tui): the OAuth, command and external methods.
    dialog.clear();
}

/// `beginKey` + `<KeyMethod>`.
fn begin_key(
    data: Rc<crate::data::Data>,
    toast: crate::toast::Toasts,
    integration: &Json,
    method: &Json,
    location: &Json,
    dialog: &crate::ui::dialog::DialogContext,
    on_connected: Option<OnConnected>,
) {
    let (data, toast) = (data.clone(), toast.clone());
    let dialog_for = dialog.clone();
    let name = get_str(integration, "name").unwrap_or_default().to_string();
    let title = get_str(method, "label").map(str::to_string).unwrap_or_else(|| format!("Connect {name}"));
    let integration = integration.clone();
    let location = location.clone();
    dialog.replace(
        move || {
            let dialog = dialog_for.clone();
            let integration = integration.clone();
            let location = location.clone();
            let on_connected = on_connected.clone();
            let (error, set_error) = create_signal::<Option<String>>(None);
            let description = {
                let error = error;
                let theme = use_theme().surface(SurfaceName::Dialog);
                Some(Rc::new(move || {
                    show(
                        move || error.get().is_some(),
                        move || {
                            let line = text();
                            line.set_fg(Some(rgba(theme.get().text.feedback.error.base)));
                            insert(
                                line,
                                View::dynamic(move || View::from(error.get().unwrap_or_default())),
                                At::Only,
                            );
                            View::Node(line)
                        },
                        None,
                    )
                }) as Rc<dyn Fn() -> View>)
            };
            let confirm = {
                let dialog = dialog.clone();
                let integration = integration.clone();
                let location = location.clone();
                let on_connected = on_connected.clone();
                let (data, toast) = (data.clone(), toast.clone());
                Rc::new(move |key: String| {
                    if key.is_empty() {
                        return;
                    }
                    let Some(id) = get_str(&integration, "id") else { return };
                    let input = obj! { "integrationID" => id, "location" => location_query(&location), "key" => key.as_str() };
                    let call = data.client.call_named("integration.connect.key", &input, RequestOptions::default());
                    let integration = integration.clone();
                    let location = location.clone();
                    let dialog = dialog.clone();
                    let on_connected = on_connected.clone();
                    let (data, toast) = (data.clone(), toast.clone());
                    tokio::task::spawn_local(async move {
                        match call.await {
                            Ok(_) => connected(&data, &toast, &integration, &location, &dialog, on_connected).await,
                            Err(error) => set_error.set(Some(error.message())),
                        }
                    });
                }) as Rc<dyn Fn(String)>
            };
            dialog_prompt(PromptProps {
                title: title.clone(),
                placeholder: Some("API key".into()),
                description,
                on_confirm: Some(confirm),
                on_cancel: Some(Rc::new(move || dialog.clear())),
                ..PromptProps::default()
            })
        },
        None,
        None,
        Some(DialogSize::Medium),
    );
}

/// `connected(integration, location, data, dialog, toast, onConnected)`.
async fn connected(
    data: &crate::data::Data,
    toast: &crate::toast::Toasts,
    integration: &Json,
    location: &Json,
    dialog: &crate::ui::dialog::DialogContext,
    on_connected: Option<OnConnected>,
) {
    let at = Some(location);
    data.store.location.integration.invalidate(at);
    data.store.location.model.invalidate(at);
    data.store.location.provider.invalidate(at);
    let (integration_sync, model_sync, provider_sync) = (
        data.store.location.integration.sync(at),
        data.store.location.model.sync(at),
        data.store.location.provider.sync(at),
    );
    let (integration_pending, model_pending, provider_pending) = (integration_sync.wait(), model_sync.wait(), provider_sync.wait());
    let mut integration_pending = std::pin::pin!(integration_pending);
    let mut model_pending = std::pin::pin!(model_pending);
    let mut provider_pending = std::pin::pin!(provider_pending);
    let (mut a, mut b, mut c) = (None, None, None);
    while a.is_none() || b.is_none() || c.is_none() {
        tokio::select! {
            biased;
            result = &mut integration_pending, if a.is_none() => a = Some(result),
            result = &mut model_pending, if b.is_none() => b = Some(result),
            result = &mut provider_pending, if c.is_none() => c = Some(result),
        }
    }
    toast.show(ToastInput::new(
        crate::toast::Variant::Success,
        format!("Connected {}", get_str(integration, "name").unwrap_or_default()),
    ));
    if let Some(on_connected) = on_connected {
        on_connected(provider_id(data, location, get_str(integration, "id").unwrap_or_default()));
        return;
    }
    dialog.clear();
}

/// `providerID(data, location, integrationID)`.
fn provider_id(data: &crate::data::Data, location: &Json, integration_id: &str) -> Option<String> {
    let models = data.store.location.model.list(Some(location)).map(|list| array(&list).to_vec()).unwrap_or_default();
    let providers = data.store.location.provider.list(Some(location)).map(|list| array(&list).to_vec()).unwrap_or_default();
    let matches: Vec<&Json> = providers
        .iter()
        .filter(|provider| get_str(provider, "integrationID") == Some(integration_id) || get_str(provider, "id") == Some(integration_id))
        .collect();
    matches
        .iter()
        .find(|provider| {
            models.iter().any(|model| {
                get_str(model, "providerID") == get_str(provider, "id") && get_str(model, "status") != Some("deprecated")
            })
        })
        .or_else(|| matches.first())
        .and_then(|provider| get_str(provider, "id").map(str::to_string))
}
