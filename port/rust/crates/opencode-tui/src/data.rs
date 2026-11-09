//! What the home screen reads from the server: the location, its agents, providers, models and configured model.
//!
//! TODO(port-tui): opencode-tui-sync's data store. A stand-in: each list is loaded once with the client and held
//! in a signal; `data.location.*` of `@opencode/client/solid` (refetch on events, per-location caches) replaces it.
//! `OPENCODE_TUI_DEBUG=<file>` appends every response to that file.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::{Signal, create_signal_with};
use opencode_tui_sync::client::{Client, RequestOptions};

/// A JSON field.
pub fn get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.as_object().and_then(|o| o.get(key))
}

pub fn get_str<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    match get(value, key) {
        Some(Json::String(s)) => Some(s),
        _ => None,
    }
}

/// A list, or the `data` list of a located response (`{ location, data }`).
pub fn array(value: &Json) -> &[Json] {
    match value {
        Json::Array(items) => items,
        Json::Object(_) => get(value, "data").map(array).unwrap_or(&[]),
        _ => &[],
    }
}

pub fn object(entries: Vec<(&str, Json)>) -> Json {
    Json::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentInfo {
    pub id: String,
    pub mode: String,
    pub hidden: bool,
    pub color: Option<String>,
    /// The agent's model (`providerID`, `id`, `variant`).
    pub model: Option<(String, String, Option<String>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    pub variants: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
}

/// The loaded lists (`undefined` until loaded).
pub struct Data {
    pub client: Client,
    pub directory: Signal<Option<String>>,
    pub agents: Signal<Option<Vec<AgentInfo>>>,
    pub models: Signal<Option<Vec<ModelInfo>>>,
    pub providers: Signal<Option<Vec<ProviderInfo>>>,
    /// The configured model (`config.model`), as `(providerID, modelID, variant)`.
    pub configured_model: Signal<Option<(String, String, Option<String>)>>,
}

fn debug(label: &str, value: &Json) {
    if let Ok(path) = std::env::var("OPENCODE_TUI_DEBUG") {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{label}: {}", opencode_tui_sync::client::stringify(value));
        }
    }
}

/// `parse("provider/model")`.
pub fn parse_model(value: &str) -> (String, String) {
    match value.split_once('/') {
        Some((provider, model)) => (provider.to_string(), model.to_string()),
        None => (value.to_string(), String::new()),
    }
}

impl Data {
    pub fn new(client: Client) -> Rc<Data> {
        Rc::new(Data {
            client,
            directory: create_signal_with(None, None).0,
            agents: create_signal_with(None, None).0,
            models: create_signal_with(None, None).0,
            providers: create_signal_with(None, None).0,
            configured_model: create_signal_with(None, None).0,
        })
    }

    async fn call(&self, name: &str, input: Json) -> Option<Json> {
        let result = self.client.call_named(name, &input, RequestOptions::default()).await;
        match result {
            Ok(value) => {
                let json = value.json();
                debug(name, &json);
                Some(json)
            }
            Err(error) => {
                debug(name, &Json::String(error.message()));
                None
            }
        }
    }

    fn location_input(&self) -> Json {
        let directory = self.directory.get_untracked().unwrap_or_default();
        object(vec![("location", object(vec![("directory", Json::String(directory))]))])
    }

    /// The location the TUI opens (app.tsx: `file.list` in the working directory, else `location.get`).
    pub async fn load_location(&self, cwd: &str) -> Option<String> {
        let input = object(vec![("location", object(vec![("directory", Json::String(cwd.to_string()))]))]);
        let mut directory = None;
        if let Some(response) = self.call("file.list", input).await {
            directory = get(&response, "location").and_then(|l| get_str(l, "directory")).map(str::to_string);
        }
        if directory.is_none() {
            if let Some(response) = self.call("location.get", Json::Object(Default::default())).await {
                directory = get_str(&response, "directory").map(str::to_string);
            }
        }
        self.directory.set(directory.clone());
        directory
    }

    /// Loads the agent, provider and model lists and the configured model of the location.
    pub async fn load_location_data(self: Rc<Self>) {
        let agents = self.call("agent.list", self.location_input()).await;
        let providers = self.call("provider.list", self.location_input()).await;
        let models = self.call("model.list", self.location_input()).await;
        let config = self.call("config.get", self.location_input()).await;
        opencode_tui_core::reactive::batch(|| {
            if let Some(agents) = agents {
                let list = array(&agents)
                    .iter()
                    .map(|a| AgentInfo {
                        id: get_str(a, "id").unwrap_or_default().to_string(),
                        mode: get_str(a, "mode").unwrap_or("primary").to_string(),
                        hidden: matches!(get(a, "hidden"), Some(Json::Bool(true))),
                        color: get_str(a, "color").map(str::to_string),
                        model: get(a, "model").map(|m| {
                            (
                                get_str(m, "providerID").unwrap_or_default().to_string(),
                                get_str(m, "id").unwrap_or_default().to_string(),
                                get_str(m, "variant").map(str::to_string),
                            )
                        }),
                    })
                    .collect();
                self.agents.set(Some(list));
            }
            if let Some(providers) = providers {
                let list = array(&providers)
                    .iter()
                    .map(|p| ProviderInfo {
                        id: get_str(p, "id").unwrap_or_default().to_string(),
                        name: get_str(p, "name").unwrap_or_default().to_string(),
                    })
                    .collect();
                self.providers.set(Some(list));
            }
            if let Some(models) = models {
                let list = array(&models)
                    .iter()
                    .map(|m| ModelInfo {
                        id: get_str(m, "id").unwrap_or_default().to_string(),
                        provider_id: get_str(m, "providerID").unwrap_or_default().to_string(),
                        name: get_str(m, "name").unwrap_or_default().to_string(),
                        variants: get(m, "variants")
                            .map(|v| array(v).iter().filter_map(|x| get_str(x, "id").map(str::to_string)).collect())
                            .unwrap_or_default(),
                    })
                    .collect();
                self.models.set(Some(list));
            }
            if let Some(config) = config {
                // The last document entry that sets a model (local.tsx `configuredModel`).
                let configured = array(&config)
                    .iter()
                    .rev()
                    .filter(|entry| get_str(entry, "type") == Some("document"))
                    .find_map(|entry| get(entry, "info").and_then(|info| get(info, "model")).cloned())
                    .or_else(|| get(&config, "model").cloned());
                let parsed = match configured {
                    Some(Json::String(value)) => {
                        let (provider, model) = parse_model(&value);
                        Some((provider, model, None))
                    }
                    Some(value @ Json::Object(_)) => Some((
                        get_str(&value, "providerID").unwrap_or_default().to_string(),
                        get_str(&value, "model").unwrap_or_default().to_string(),
                        get_str(&value, "variant").map(str::to_string),
                    )),
                    _ => None,
                };
                self.configured_model.set(parsed);
            }
        });
    }
}
