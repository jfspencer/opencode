//! The TUI's data (`context/data.tsx` over `@opencode/client/solid`'s `createData`): `opencode-tui-sync`'s store,
//! fed by the client connection's event stream, observed through the reactive layer (its `Reactivity` keys map to
//! signals), and the views of it the home screen reads (agents, providers, models, the configured model, the
//! location's directory) as memos over the store's reads.
//!
//! The providers are opencode-tui-sync's contexts, built in TS's order: [`Data::client_context`] (`useClient()`),
//! [`Data::store`] (`useData()`: the store with `location.sync` and `session.title`), [`Data::location`]
//! (`useLocation()`), [`Data::event`] (`useEvent()`). The views below are what the home screen reads.
//! `OPENCODE_TUI_DEBUG=<file>` appends the direct responses to that file.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::{KeyedSignals, Memo, Signal, create_memo_equals, create_signal_with};
use opencode_tui_sync::client::{Client, RequestOptions};
use opencode_tui_sync::connection::ClientConnection;
use opencode_tui_sync::context::client::{ClientContext, ClientInput};
use opencode_tui_sync::context::data::DataContext;
use opencode_tui_sync::context::event::EventContext;
use opencode_tui_sync::context::location::LocationContext;
use opencode_tui_sync::reactive::{Key, Reactivity};

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
    pub description: Option<String>,
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

/// The store's change notification as signals: a read tracks its key; a change triggers the key, its ancestors
/// and its descendants (opencode-tui-sync's `Reactivity` contract).
struct Signals(KeyedSignals<Key>);

fn under(key: &Key, ancestor: &Key) -> bool {
    let mut parent = key.parent();
    while let Some(p) = parent {
        if &p == ancestor {
            return true;
        }
        parent = p.parent();
    }
    false
}

impl Reactivity for Signals {
    fn track(&self, key: &Key) {
        self.0.track(key);
    }
    fn trigger(&self, key: &Key) {
        let signals = &self.0;
        signals.batch(&mut || {
            signals.trigger(key);
            let mut parent = key.parent();
            while let Some(p) = parent {
                signals.trigger(&p);
                parent = p.parent();
            }
            for descendant in signals.keys_where(|k| under(k, key)) {
                signals.trigger(&descendant);
            }
        });
    }
    fn batch(&self, f: &mut dyn FnMut()) {
        self.0.batch(f);
    }
    fn untrack(&self, f: &mut dyn FnMut()) {
        self.0.untrack(f);
    }
}

/// The data the TUI reads.
pub struct Data {
    pub client: Client,
    /// `useClient()`: the client, its event bus, the connection, media.
    pub client_context: ClientContext,
    /// `useData()`: the store (derefs to `createData`'s data).
    pub store: DataContext,
    /// `useLocation()`.
    pub location: LocationContext,
    /// `useEvent()`.
    pub event: EventContext,
    /// The event stream and its reconnects (`client.connection`).
    pub connection: ClientConnection,
    pub directory: Signal<Option<String>>,
    pub agents: Memo<Option<Vec<AgentInfo>>>,
    pub models: Memo<Option<Vec<ModelInfo>>>,
    pub providers: Memo<Option<Vec<ProviderInfo>>>,
    /// The configured model (`config.model`), as `(providerID, modelID, variant)`.
    pub configured_model: Memo<Option<(String, String, Option<String>)>>,
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

fn location_of(directory: &Signal<Option<String>>) -> Option<Json> {
    directory.get().map(|d| object(vec![("directory", Json::String(d))]))
}

fn agents_of(list: &Json) -> Vec<AgentInfo> {
    array(list)
        .iter()
        .map(|a| AgentInfo {
            id: get_str(a, "id").unwrap_or_default().to_string(),
            description: get_str(a, "description").map(str::to_string),
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
        .collect()
}

fn models_of(list: &Json) -> Vec<ModelInfo> {
    array(list)
        .iter()
        .map(|m| ModelInfo {
            id: get_str(m, "id").unwrap_or_default().to_string(),
            provider_id: get_str(m, "providerID").unwrap_or_default().to_string(),
            name: get_str(m, "name").unwrap_or_default().to_string(),
            variants: get(m, "variants")
                .map(|v| array(v).iter().filter_map(|x| get_str(x, "id").map(str::to_string)).collect())
                .unwrap_or_default(),
        })
        .collect()
}

fn providers_of(list: &Json) -> Vec<ProviderInfo> {
    array(list)
        .iter()
        .map(|p| ProviderInfo {
            id: get_str(p, "id").unwrap_or_default().to_string(),
            name: get_str(p, "name").unwrap_or_default().to_string(),
        })
        .collect()
}

/// local.tsx `configuredModel`: the last document entry that sets a model.
fn configured_of(config: &Json) -> Option<(String, String, Option<String>)> {
    let configured = array(config)
        .iter()
        .rev()
        .filter(|entry| get_str(entry, "type") == Some("document"))
        .find_map(|entry| get(entry, "info").and_then(|info| get(info, "model")).cloned());
    match configured {
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
    }
}

impl Data {
    /// The store for a client (`ClientProvider` + `DataProvider`), its connection started.
    pub fn new(client: Client, url: &str, headers: Vec<(String, String)>, directory: &str) -> Rc<Data> {
        let reactivity: Rc<dyn Reactivity> = Rc::new(Signals(KeyedSignals::new()));
        let client_context = ClientContext::new(ClientInput {
            api: client.clone(),
            url: Some(url.to_string()),
            headers: Some(headers),
            service: None,
            log: None,
            reactivity,
        });
        let store = DataContext::new(&client_context, directory);
        let location = LocationContext::new(&client_context, &store);
        let event = EventContext::new(&client_context);
        let connection = client_context.connection().clone();
        let directory = create_signal_with(None, None).0;
        let agents = {
            let resource = store.location.agent.clone();
            create_memo_equals(move || location_of(&directory).and_then(|l| resource.list(Some(&l))).map(|r| agents_of(&r)), None)
        };
        let models = {
            let resource = store.location.model.clone();
            create_memo_equals(move || location_of(&directory).and_then(|l| resource.list(Some(&l))).map(|r| models_of(&r)), None)
        };
        let providers = {
            let resource = store.location.provider.clone();
            create_memo_equals(
                move || location_of(&directory).and_then(|l| resource.list(Some(&l))).map(|r| providers_of(&r)),
                None,
            )
        };
        let configured_model = {
            let resource = store.location.config.clone();
            create_memo_equals(
                move || location_of(&directory).and_then(|l| resource.list(Some(&l))).and_then(|r| configured_of(&r)),
                None,
            )
        };
        let _ = connection.start();
        Rc::new(Data {
            client,
            client_context,
            store,
            location,
            event,
            connection,
            directory,
            agents,
            models,
            providers,
            configured_model,
        })
    }

    async fn call(&self, name: &str, input: Json) -> Option<Json> {
        match self.client.call_named(name, &input, RequestOptions::default()).await {
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

    /// Loads the location's agents, providers, models and config into the store.
    pub async fn load_location_data(self: Rc<Self>) {
        let Some(location) = self.directory.get_untracked().map(|d| object(vec![("directory", Json::String(d))])) else {
            return;
        };
        // context/data.tsx: `Promise.all([data.location.sync(ref), data.location.config.sync(ref)])`.
        let loads = [self.store.location.sync(Some(&location)), self.store.location.config.sync(Some(&location))];
        for load in loads {
            let _ = load.wait().await;
        }
    }
}
