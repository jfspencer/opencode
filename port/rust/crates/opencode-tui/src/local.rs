//! `context/local.tsx` (`LocalProvider` / `useLocal`): the agent selection (current, set, move, colors), the model
//! selection (the current model and variant of a new session or of the open session, per agent; recent and favorite
//! models and remembered variants in model.json through [`crate::model_preference`]; cycling; per-session drafts
//! reconciled with the server's `session.model.selected`/`session.agent.selected` commits), the pinned sessions and
//! quick switch (session.json), and the prompt's display of the selection (`promptDisplay`).
//!
//! [`install`] creates the one instance under the app's root owner (the provider); [`local`] returns it.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::RGBA;
use opencode_tui_core::reactive::*;

use crate::ctx::{Route, tui};
use crate::data::{AgentInfo, Data, ModelInfo, get, get_str};
use crate::model_preference::{
    ModelPreference, ModelRef, Repository, cycle_model_variant, favorite_models, model_preference_key, normalize_model_variant,
    recent_models,
};
use crate::util::{rgba, titlecase};

/// What the prompt shows of the selection.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub agent: Option<String>,
    pub model: String,
    pub provider: String,
    pub variant: Option<String>,
}

/// `ModelSelection`: a model and its variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub provider_id: String,
    pub model_id: String,
    pub variant: Option<String>,
}

impl Selection {
    pub fn model(&self) -> ModelRef {
        ModelRef::new(self.provider_id.clone(), self.model_id.clone())
    }
}

/// `local.model.parsed()`.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub provider: String,
    pub model: String,
    pub reasoning: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Preferences {
    ready: bool,
    preference: ModelPreference,
}

/// `selectionKey(value)`.
fn selection_key(value: &Selection) -> String {
    format!(
        "{}:{}",
        model_preference_key(&value.model()),
        normalize_model_variant(value.variant.as_deref()).unwrap_or_else(|| "default".into())
    )
}

struct Inner {
    data: Rc<Data>,
    toast: crate::toast::Toasts,
    theme: Signal<crate::theme::ResolvedTheme>,
    route: Signal<Route>,
    config: crate::config_context::ConfigContext,
    // agent
    agents: Memo<Vec<AgentInfo>>,
    visible_agents: Memo<Vec<AgentInfo>>,
    agent_current: Signal<Option<String>>,
    /// `draftBySession`: `Some(None)` is an entry without an agent (`{ agent: undefined }`).
    agent_drafts: Signal<HashMap<String, Option<String>>>,
    // model
    preferences: Signal<Preferences>,
    new_session_model: Signal<HashMap<String, ModelRef>>,
    session_selection: Signal<HashMap<String, HashMap<String, Option<Selection>>>>,
    repository: Repository,
    pending_commits: RefCell<HashMap<String, Rc<(String, String)>>>,
    configured: Memo<Option<Selection>>,
    fallback: Memo<Option<ModelRef>>,
    current_selection: std::cell::OnceCell<Memo<Option<Selection>>>,
    // session
    pinned: Signal<Vec<String>>,
    pinned_ready: Signal<bool>,
    pinned_pending: std::cell::Cell<bool>,
    pinned_path: PathBuf,
}

/// `useLocal()`.
#[derive(Clone)]
pub struct Local(Rc<Inner>);

thread_local! {
    static LOCAL: RefCell<Option<Local>> = const { RefCell::new(None) };
}

/// `useLocal()`.
pub fn local() -> Local {
    LOCAL.with(|l| l.borrow().clone()).unwrap_or_else(install)
}

fn session_record(data: &Data, session: &str) -> Option<Rc<Json>> {
    data.store.session.get(session)
}

/// `durableSelection(sessionID)`: the session's committed model.
fn durable_selection(data: &Data, session: &str) -> Option<Selection> {
    let record = session_record(data, session)?;
    let model = get(&record, "model")?;
    Some(Selection {
        provider_id: get_str(model, "providerID")?.to_string(),
        model_id: get_str(model, "id")?.to_string(),
        variant: normalize_model_variant(get_str(model, "variant")),
    })
}

fn session_agent(data: &Data, session: &str) -> Option<String> {
    session_record(data, session).and_then(|r| get_str(&r, "agent").map(str::to_string))
}

/// `LocalProvider`'s init, under the current owner.
pub fn install() -> Local {
    let t = tui();
    let data = t.data.clone();
    let args = t.args.clone();
    let route = t.route;
    let agents = {
        let data = data.clone();
        create_memo(move || data.agents.get().unwrap_or_default().into_iter().filter(|a| a.mode != "subagent" && !a.hidden).collect())
    };
    let visible_agents = {
        let data = data.clone();
        create_memo(move || data.agents.get().unwrap_or_default().into_iter().filter(|a| !a.hidden).collect())
    };
    let agent_current = create_signal(None).0;
    let agent_drafts: Signal<HashMap<String, Option<String>>> = create_signal(HashMap::new()).0;
    let preferences = create_signal(Preferences::default()).0;
    let new_session_model: Signal<HashMap<String, ModelRef>> = create_signal(HashMap::new()).0;
    let session_selection: Signal<HashMap<String, HashMap<String, Option<Selection>>>> = create_signal(HashMap::new()).0;
    let repository = Repository::new(PathBuf::from(&t.paths.state).join("model.json"));
    let configured = {
        let data = data.clone();
        create_memo(move || {
            data.configured_model.get().map(|(provider_id, model_id, variant)| Selection { provider_id, model_id, variant })
        })
    };
    let models = {
        let data = data.clone();
        move || data.models.get()
    };
    let valid = {
        let models = models.clone();
        move |model: &ModelRef| models().is_some_and(|list| list.iter().any(|m| m.provider_id == model.provider_id && m.id == model.model_id))
    };
    let fallback = {
        let models = models.clone();
        let valid = valid.clone();
        let args = args.clone();
        create_memo(move || {
            if let Some(value) = &args.model {
                let (provider_id, model_id) = crate::data::parse_model(value);
                let model = ModelRef::new(provider_id, model_id);
                if valid(&model) {
                    return Some(model);
                }
            }
            if let Some(configured) = configured.get() {
                if valid(&configured.model()) {
                    return Some(configured.model());
                }
            }
            for item in preferences.with(|p| p.preference.recent.clone()) {
                if valid(&item) {
                    return Some(item);
                }
            }
            let first = models()?.first().cloned()?;
            Some(ModelRef::new(first.provider_id, first.id))
        })
    };
    let pinned_path = PathBuf::from(&t.paths.state).join("session.json");
    // The selection reads the agent through the instance: its memo is made once the instance exists.
    let inner = Rc::new(Inner {
        data: data.clone(),
        toast: t.toast.clone(),
        theme: t.theme,
        route,
        config: t.config_context.clone(),
        agents,
        visible_agents,
        agent_current,
        agent_drafts,
        preferences,
        new_session_model,
        session_selection,
        repository: repository.clone(),
        pending_commits: RefCell::new(HashMap::new()),
        configured,
        fallback,
        current_selection: std::cell::OnceCell::new(),
        pinned: create_signal(Vec::new()).0,
        pinned_ready: create_signal(false).0,
        pinned_pending: std::cell::Cell::new(false),
        pinned_path,
    });
    let local = Local(inner);
    LOCAL.with(|l| *l.borrow_mut() = Some(local.clone()));
    // `currentSelection`.
    let current_selection = {
        let local = local.clone();
        create_memo(move || {
            let route = local.0.route.get();
            if let Route::Session { session_id } = route {
                return local.session_selection_of(&session_id);
            }
            let model = local.new_session_model()?;
            Some(local.preferred_selection(&model))
        })
    };
    let _ = local.0.current_selection.set(current_selection);

    // model.json, kept in the preferences store.
    {
        let preferences = local.0.preferences;
        let unsubscribe = repository.subscribe(move |value| {
            batch(|| preferences.set(Preferences { ready: true, preference: value }));
        });
        on_cleanup(unsubscribe);
    }
    // Server commits and deletions.
    {
        let event = data.event.clone();
        let deleted = {
            let local = local.clone();
            event.on("session.deleted", move |evt| {
                let Some(id) = get_str(evt.data(), "sessionID").map(str::to_string) else { return };
                local.0.agent_drafts.update(|d| {
                    d.remove(&id);
                });
                local.0.pending_commits.borrow_mut().remove(&id);
                local.0.session_selection.update(|s| {
                    s.remove(&id);
                });
                local.prune_pinned(&id);
            })
        };
        let agent_selected = {
            let local = local.clone();
            event.on("session.agent.selected", move |evt| {
                let data = evt.data();
                let Some(id) = get_str(data, "sessionID").map(str::to_string) else { return };
                let agent = get_str(data, "agent").map(str::to_string);
                // Keep an entry after acknowledgment: CLI defaults must not override a user's later choice.
                let matches = local.0.agent_drafts.with_untracked(|d| d.get(&id).is_some_and(|a| a.is_some() && *a == agent));
                if matches {
                    local.0.agent_drafts.update(|d| {
                        d.insert(id.clone(), None);
                    });
                }
                local.reconcile_session_selection(&id);
            })
        };
        let model_selected = {
            let local = local.clone();
            event.on("session.model.selected", move |evt| {
                if let Some(id) = get_str(evt.data(), "sessionID").map(str::to_string) {
                    local.reconcile_session_selection(&id);
                }
            })
        };
        on_cleanup(move || {
            deleted.call();
            agent_selected.call();
            model_selected.call();
        });
    }
    local.load_pinned();
    local
}

impl Local {
    fn models(&self) -> Option<Vec<ModelInfo>> {
        self.0.data.models.get()
    }

    fn model_info(&self, model: &ModelRef) -> Option<ModelInfo> {
        self.models()?.into_iter().find(|m| m.provider_id == model.provider_id && m.id == model.model_id)
    }

    /// `isModelValid(model)`.
    pub fn is_model_valid(&self, model: &ModelRef) -> bool {
        self.models().is_some_and(|list| list.iter().any(|m| m.provider_id == model.provider_id && m.id == model.model_id))
    }

    fn session_route(&self) -> Option<String> {
        match self.0.route.get() {
            Route::Session { session_id } => Some(session_id),
            Route::Home => None,
        }
    }

    // ---- agent

    /// `local.agent.list()`.
    pub fn agent_list(&self) -> Vec<AgentInfo> {
        self.0.agents.get()
    }

    /// `local.agent.ready`.
    pub fn agent_ready(&self) -> bool {
        self.0.data.agents.with(|a| a.is_some())
    }

    /// `local.agent.current()`.
    pub fn agent_current(&self) -> Option<AgentInfo> {
        let agents = self.0.agents.get();
        let selected = match self.session_route() {
            Some(session) => {
                let draft = self.0.agent_drafts.with(|d| d.get(&session).cloned());
                match draft {
                    Some(Some(agent)) => Some(agent),
                    Some(None) => session_agent(&self.0.data, &session).or_else(|| self.0.agent_current.get()),
                    None => tui()
                        .args
                        .agent
                        .clone()
                        .or_else(|| session_agent(&self.0.data, &session))
                        .or_else(|| self.0.agent_current.get()),
                }
            }
            None => self.0.agent_current.get(),
        };
        selected.and_then(|id| agents.iter().find(|a| a.id == id).cloned()).or_else(|| agents.first().cloned())
    }

    /// `local.agent.set(id)`.
    pub fn agent_set(&self, id: &str) {
        if !self.0.agents.get_untracked().iter().any(|a| a.id == id) {
            self.0.toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Warning, format!("Agent not found: {id}")).duration(3000));
            return;
        }
        batch(|| {
            let changed = untrack(|| self.agent_current()).is_none_or(|a| a.id != id);
            if changed {
                self.remember();
            }
            self.0.agent_current.set(Some(id.to_string()));
            let session = untrack(|| self.session_route());
            if let Some(session) = &session {
                self.0.agent_drafts.update(|d| {
                    d.insert(session.clone(), Some(id.to_string()));
                });
            }
            // Retain both selections while agent and model commits arrive separately.
            let selected = if changed && session.is_some() { untrack(|| self.model_current()) } else { None };
            if let Some(selected) = selected {
                self.model_set(&selected, false);
            }
        });
    }

    /// `local.agent.move(direction)`.
    pub fn agent_move(&self, direction: i64) {
        batch(|| {
            let Some(current) = untrack(|| self.agent_current()) else { return };
            let agents = self.0.agents.get_untracked();
            let count = agents.len() as i64;
            let mut next = agents.iter().position(|a| a.id == current.id).map(|i| i as i64).unwrap_or(-1) + direction;
            if next < 0 {
                next = count - 1;
            }
            if next >= count {
                next = 0;
            }
            if let Some(value) = agents.get(next as usize) {
                self.agent_set(&value.id.clone());
            }
        });
    }

    /// The categorical colors (`colors()`): the new designs' vivid 400 steps, else 200, deduplicated.
    fn colors(&self) -> Vec<RGBA> {
        let step = if self.0.config.data().new_designs() { 400 } else { 200 };
        let theme = self.0.theme.get();
        let mut out: Vec<RGBA> = Vec::new();
        for scale in theme.categorical.iter() {
            let color = rgba(scale[step]);
            if !out.contains(&color) {
                out.push(color);
            }
        }
        out
    }

    /// `local.agent.color(id)`: the agent's color, else the theme's categorical color of its index.
    pub fn color(&self, id: &str) -> RGBA {
        let colors = self.colors();
        let fallback = colors.first().copied().unwrap_or(RGBA::from_values(1.0, 1.0, 1.0, 1.0));
        let visible = self.0.visible_agents.get();
        let Some(index) = visible.iter().position(|a| a.id == id) else { return fallback };
        if let Some(color) = visible[index].color.as_deref() {
            return RGBA::from_hex(color);
        }
        colors.get(index % colors.len().max(1)).copied().unwrap_or(fallback)
    }

    /// `Locale.titlecase(agent.id)`.
    pub fn agent_label(&self) -> Option<String> {
        self.agent_current().map(|a| titlecase(&a.id))
    }

    /// The current agent's color.
    pub fn agent_color(&self) -> RGBA {
        match self.agent_current() {
            Some(agent) => self.color(&agent.id),
            None => self.colors().first().copied().unwrap_or(RGBA::from_values(1.0, 1.0, 1.0, 1.0)),
        }
    }

    // ---- model

    fn location_agent_key(&self, agent: &str) -> String {
        let directory = self.0.data.directory.get().unwrap_or_default();
        format!("{}:{agent}", opencode_tui_sync::client::stringify(&Json::Array(vec![Json::String(directory), Json::Null])))
    }

    /// `newSessionModel()`.
    fn new_session_model(&self) -> Option<ModelRef> {
        let agent = self.agent_current();
        let remembered = agent.as_ref().and_then(|a| self.0.new_session_model.with(|m| m.get(&self.location_agent_key(&a.id)).cloned()));
        if let Some(model) = remembered.filter(|m| self.is_model_valid(m)) {
            return Some(model);
        }
        let configured = agent.and_then(|a| a.model).map(|(p, m, _)| ModelRef::new(p, m));
        if let Some(model) = configured.filter(|m| self.is_model_valid(m)) {
            return Some(model);
        }
        self.0.fallback.get().filter(|m| self.is_model_valid(m))
    }

    /// `preferredSelection(model)`.
    fn preferred_selection(&self, model: &ModelRef) -> Selection {
        let configured = self.agent_current().and_then(|a| a.model);
        let fallback = self.0.configured.get();
        let preferred = self.0.preferences.with(|p| p.preference.variant_of(&model_preference_key(model)).map(str::to_string));
        let variant = preferred
            .or_else(|| {
                configured
                    .filter(|(p, m, _)| *p == model.provider_id && *m == model.model_id)
                    .and_then(|(_, _, v)| v)
            })
            .or_else(|| fallback.filter(|f| f.provider_id == model.provider_id && f.model_id == model.model_id).and_then(|f| f.variant));
        let variant = normalize_model_variant(variant.as_deref());
        let info = self.model_info(model);
        Selection {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            variant: variant.filter(|v| info.is_some_and(|i| i.variants.iter().any(|x| x == v))),
        }
    }

    /// `sessionSelection(sessionID)`.
    fn session_selection_of(&self, session: &str) -> Option<Selection> {
        let current = self.agent_current()?;
        let session_agent = session_agent(&self.0.data, session);
        let selected = self
            .0
            .session_selection
            .with(|s| s.get(session).and_then(|m| m.get(&current.id).cloned()).flatten())
            .or_else(|| {
                if session_agent.is_none() || session_agent.as_deref() == Some(current.id.as_str()) {
                    durable_selection(&self.0.data, session)
                } else {
                    None
                }
            });
        if let Some(selected) = selected {
            let info = self.model_info(&selected.model());
            let keep = info.is_none_or(|i| selected.variant.as_ref().is_none_or(|v| i.variants.iter().any(|x| x == v)));
            return Some(Selection { variant: if keep { selected.variant.clone() } else { None }, ..selected });
        }
        let model = self.new_session_model()?;
        Some(self.preferred_selection(&model))
    }

    fn set_session_selection(&self, session: &str, agent: &str, selection: Option<Selection>) {
        self.0.session_selection.update(|s| {
            s.entry(session.to_string()).or_default().insert(agent.to_string(), selection);
        });
    }

    /// `setSessionDraft(sessionID, selection)`.
    fn set_session_draft(&self, session: &str, selection: Selection) {
        let Some(current) = untrack(|| self.agent_current()) else { return };
        let durable = durable_selection(&self.0.data, session);
        let agent = session_agent(&self.0.data, session);
        let same = (agent.is_none() || agent.as_deref() == Some(current.id.as_str()))
            && durable.as_ref().is_some_and(|d| selection_key(d) == selection_key(&selection));
        self.set_session_selection(session, &current.id, if same { None } else { Some(selection) });
    }

    /// `selectModel(model)`.
    fn select_model(&self, model: &ModelRef) -> bool {
        if let Some(session) = untrack(|| self.session_route()) {
            let current = untrack(|| self.session_selection_of(&session));
            let selection = match current {
                Some(current) if current.provider_id == model.provider_id && current.model_id == model.model_id => current,
                _ => untrack(|| self.preferred_selection(model)),
            };
            self.set_session_draft(&session, selection);
            return true;
        }
        let Some(current) = untrack(|| self.agent_current()) else { return false };
        let key = untrack(|| self.location_agent_key(&current.id));
        self.0.new_session_model.update(|m| {
            m.insert(key, model.clone());
        });
        true
    }

    /// `reconcileSessionSelection(sessionID)`.
    fn reconcile_session_selection(&self, session: &str) {
        let expected = self.0.pending_commits.borrow().get(session).cloned();
        let Some(expected) = expected else { return };
        let Some(durable) = durable_selection(&self.0.data, session) else { return };
        if session_agent(&self.0.data, session).as_deref() != Some(expected.0.as_str()) {
            return;
        }
        if selection_key(&durable) != expected.1 {
            return;
        }
        self.0.pending_commits.borrow_mut().remove(session);
        // Inactive agents keep their remembered choices after another agent commits.
        let route = untrack(|| self.session_route());
        if route.as_deref() != Some(session) || untrack(|| self.agent_current()).is_none_or(|a| a.id != expected.0) {
            return;
        }
        let draft = self.0.session_selection.with_untracked(|s| s.get(session).and_then(|m| m.get(&expected.0).cloned()).flatten());
        if draft.is_some_and(|d| selection_key(&d) == expected.1) {
            self.set_session_selection(session, &expected.0, None);
        }
    }

    /// `local.model.current()`.
    pub fn model_current(&self) -> Option<ModelRef> {
        self.model_selection().map(|s| s.model())
    }

    /// `local.model.selection()`.
    pub fn model_selection(&self) -> Option<Selection> {
        self.0.current_selection.get().and_then(|m| m.get())
    }

    /// `local.model.remember()`: the open session keeps the current agent's selection.
    pub fn remember(&self) {
        let Some(current) = untrack(|| self.agent_current()) else { return };
        let Some(selection) = untrack(|| self.model_selection()) else { return };
        let Some(session) = untrack(|| self.session_route()) else { return };
        self.set_session_selection(&session, &current.id, Some(selection));
    }

    /// `local.model.available(model)`.
    pub fn available(&self, model: Option<&ModelRef>) -> bool {
        match model {
            Some(model) => self.is_model_valid(model),
            None => self.model_current().is_some_and(|m| self.is_model_valid(&m)),
        }
    }

    /// `local.model.trackSessionCommit(sessionID, model, agentID)`: the returned function forgets the commit.
    pub fn track_session_commit(&self, session: &str, provider_id: &str, model_id: &str, variant: Option<&str>, agent: &str) -> Box<dyn FnOnce()> {
        let selection = Selection { provider_id: provider_id.into(), model_id: model_id.into(), variant: variant.map(str::to_string) };
        let committed = Rc::new((agent.to_string(), selection_key(&selection)));
        self.0.pending_commits.borrow_mut().insert(session.to_string(), committed.clone());
        // An unchanged model emits no event; the agent may be the only durable change.
        self.reconcile_session_selection(session);
        let local = self.clone();
        let session = session.to_string();
        Box::new(move || {
            let mut pending = local.0.pending_commits.borrow_mut();
            if pending.get(&session).is_some_and(|c| Rc::ptr_eq(c, &committed)) {
                pending.remove(&session);
            }
        })
    }

    /// `local.model.ready`: model.json has loaded.
    pub fn model_ready(&self) -> bool {
        self.0.preferences.with(|p| p.ready)
    }

    /// `local.model.catalogReady`.
    pub fn catalog_ready(&self) -> bool {
        self.0.data.models.with(|m| m.is_some())
    }

    /// `local.model.recent()`.
    pub fn recent(&self) -> Vec<ModelRef> {
        self.0.preferences.with(|p| p.preference.recent.clone())
    }

    /// `local.model.favorite()`.
    pub fn favorite(&self) -> Vec<ModelRef> {
        self.0.preferences.with(|p| p.preference.favorite.clone())
    }

    /// `local.model.parsed()`.
    pub fn parsed(&self) -> Parsed {
        let Some(value) = self.model_selection() else {
            return Parsed { provider: "Connect a provider".into(), model: "No provider selected".into(), reasoning: false };
        };
        let provider = self
            .0
            .data
            .providers
            .get()
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.id == value.provider_id)
            .map(|p| p.name)
            .unwrap_or(value.provider_id.clone());
        let info = self.model_info(&value.model());
        Parsed {
            provider,
            model: info.as_ref().map(|i| i.name.clone()).unwrap_or(format!("{} (unavailable)", value.model_id)),
            reasoning: info.is_some_and(|i| !i.variants.is_empty()),
        }
    }

    /// `local.model.cycle(direction)`: the next recent model.
    pub fn cycle(&self, direction: i64) {
        let Some(current) = untrack(|| self.model_selection()) else { return };
        let recent: Vec<ModelRef> = untrack(|| self.recent().into_iter().filter(|m| self.is_model_valid(m)).collect());
        let count = recent.len() as i64;
        let index = recent.iter().position(|m| m.provider_id == current.provider_id && m.model_id == current.model_id);
        let mut next = match index {
            None => if direction == 1 { 0 } else { count - 1 },
            Some(i) => i as i64 + direction,
        };
        if next < 0 {
            next = count - 1;
        }
        if next >= count {
            next = 0;
        }
        let Some(value) = recent.get(next.max(0) as usize).cloned() else { return };
        self.select_model(&value);
    }

    /// `local.model.cycleFavorite(direction)`.
    pub fn cycle_favorite(&self, direction: i64) {
        let favorites: Vec<ModelRef> = untrack(|| self.favorite().into_iter().filter(|m| self.is_model_valid(m)).collect());
        if favorites.is_empty() {
            self.0.toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Info, "Add a favorite model to use this shortcut").duration(3000));
            return;
        }
        let current = untrack(|| self.model_selection());
        let count = favorites.len() as i64;
        let mut index = current
            .and_then(|c| favorites.iter().position(|m| m.provider_id == c.provider_id && m.model_id == c.model_id))
            .map(|i| i as i64)
            .unwrap_or(-1);
        if index == -1 {
            index = if direction == 1 { 0 } else { count - 1 };
        } else {
            index += direction;
            if index < 0 {
                index = count - 1;
            }
            if index >= count {
                index = 0;
            }
        }
        let Some(next) = favorites.get(index as usize).cloned() else { return };
        if !self.select_model(&next) {
            return;
        }
        let recent = recent_models(&next, &self.0.preferences.with_untracked(|p| p.preference.recent.clone()));
        self.0.preferences.update(|p| p.preference.recent = recent);
        self.0.repository.add_recent(next);
    }

    /// `local.model.set(model, { recent })`.
    pub fn model_set(&self, model: &ModelRef, recent: bool) {
        batch(|| {
            if !untrack(|| self.is_model_valid(model)) {
                return;
            }
            if !self.select_model(model) {
                return;
            }
            if recent {
                let list = recent_models(model, &self.0.preferences.with_untracked(|p| p.preference.recent.clone()));
                self.0.preferences.update(|p| p.preference.recent = list);
                self.0.repository.add_recent(model.clone());
            }
        });
    }

    /// `local.model.toggleFavorite(model)`.
    pub fn toggle_favorite(&self, model: &ModelRef) {
        batch(|| {
            if !untrack(|| self.is_model_valid(model)) {
                return;
            }
            let favorite = self.0.preferences.with_untracked(|p| p.preference.favorite.clone());
            let exists = favorite.iter().any(|m| m == model);
            let next = favorite_models(model, &favorite, !exists);
            self.0.preferences.update(|p| p.preference.favorite = next);
            self.0.repository.set_favorite(model.clone(), !exists);
        });
    }

    /// `local.model.variant.current()`.
    pub fn variant_current(&self) -> Option<String> {
        self.model_selection().and_then(|s| s.variant)
    }

    /// `local.model.variant.list()`: the current model's variants.
    pub fn variant_list(&self) -> Vec<String> {
        let Some(selection) = self.model_selection() else { return Vec::new() };
        self.model_info(&selection.model()).map(|m| m.variants).unwrap_or_default()
    }

    /// `local.model.variant.set(value)`.
    pub fn variant_set(&self, value: Option<&str>) {
        let Some(current) = untrack(|| self.model_selection()) else { return };
        if let Some(session) = untrack(|| self.session_route()) {
            self.set_session_draft(&session, Selection { variant: normalize_model_variant(value), ..current.clone() });
        }
        let key = model_preference_key(&current.model());
        let stored = value.unwrap_or("default").to_string();
        self.0.preferences.update(|p| match p.preference.variant.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = stored.clone(),
            None => p.preference.variant.push((key.clone(), stored.clone())),
        });
        self.0.repository.save_variant(current.model(), value.map(str::to_string));
    }

    /// `local.model.variant.cycle()`.
    pub fn variant_cycle(&self) {
        let variants = untrack(|| self.variant_list());
        if variants.is_empty() {
            return;
        }
        let next = cycle_model_variant(untrack(|| self.variant_current()).as_deref(), &variants);
        self.variant_set(next.as_deref());
    }

    /// `promptDisplay()`.
    pub fn display(&self) -> Display {
        let Some(selection) = self.model_selection() else {
            return Display {
                agent: self.agent_label(),
                model: "No provider selected".into(),
                provider: "Connect a provider".into(),
                variant: None,
            };
        };
        let parsed = self.parsed();
        Display { agent: self.agent_label(), model: parsed.model, provider: parsed.provider, variant: selection.variant }
    }

    /// Whether the lists the display needs have loaded (`loading` in promptDisplay).
    pub fn ready(&self) -> bool {
        self.0.data.agents.with(|a| a.is_some()) && self.0.data.models.with(|m| m.is_some())
    }

    // ---- session

    fn load_pinned(&self) {
        let path = self.0.pinned_path.clone();
        let local = self.clone();
        tokio::task::spawn_local(async move {
            let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(path).ok()).await.ok().flatten();
            let parsed = text.and_then(|t| opencode_protocol::wire::json::parse(&t).ok());
            if let Some(Json::Array(items)) = parsed.as_ref().and_then(|v| v.as_object()).and_then(|o| o.get("pinned")) {
                let pinned = items.iter().filter_map(|i| match i {
                    Json::String(s) => Some(s.clone()),
                    _ => None,
                });
                local.0.pinned.set(pinned.collect());
            }
            local.0.pinned_ready.set(true);
            if local.0.pinned_pending.get() {
                local.save_pinned();
            }
        });
    }

    fn save_pinned(&self) {
        if !self.0.pinned_ready.get_untracked() {
            self.0.pinned_pending.set(true);
            return;
        }
        self.0.pinned_pending.set(false);
        let pinned = Json::Array(self.0.pinned.get_untracked().into_iter().map(Json::String).collect());
        let value = Json::Object([("pinned".to_string(), pinned)].into_iter().collect());
        let path = self.0.pinned_path.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let temporary = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
            if std::fs::write(&temporary, opencode_tui_sync::client::stringify(&value)).is_ok() {
                let _ = std::fs::rename(&temporary, &path);
            }
        });
    }

    fn prune_pinned(&self, session: &str) {
        batch(|| {
            if self.0.pinned.with_untracked(|p| p.iter().any(|x| x == session)) {
                self.0.pinned.update(|p| p.retain(|x| x != session));
            }
            self.save_pinned();
        });
    }

    /// `local.session.ready`.
    pub fn pinned_ready(&self) -> bool {
        self.0.pinned_ready.get()
    }

    /// `local.session.pinned()`.
    pub fn pinned(&self) -> Vec<String> {
        self.0.pinned.get()
    }

    /// `local.session.slots()`: the pinned top-level sessions that exist, at most 9.
    pub fn slots(&self) -> Vec<String> {
        let existing: Vec<String> = self
            .0
            .data
            .store
            .session
            .list()
            .iter()
            .filter(|s| get(s, "parentID").is_none_or(|p| matches!(p, Json::Null)))
            .filter_map(|s| get_str(s, "id").map(str::to_string))
            .collect();
        self.pinned().into_iter().filter(|id| existing.contains(id)).take(9).collect()
    }

    /// `local.session.isPinned(sessionID)`.
    pub fn is_pinned(&self, session: &str) -> bool {
        self.0.pinned.with(|p| p.iter().any(|x| x == session))
    }

    /// `local.session.togglePin(sessionID)`.
    pub fn toggle_pin(&self, session: &str) {
        batch(|| {
            let exists = self.0.pinned.with_untracked(|p| p.iter().any(|x| x == session));
            self.0.pinned.update(|p| {
                if exists {
                    p.retain(|x| x != session);
                } else {
                    p.push(session.to_string());
                }
            });
            self.save_pinned();
        });
    }

    /// `local.session.quickSwitch(slot)`.
    pub fn quick_switch(&self, slot: usize) {
        let Some(target) = untrack(|| self.slots()).get(slot.wrapping_sub(1)).cloned() else { return };
        if untrack(|| self.session_route()).as_deref() == Some(target.as_str()) {
            return;
        }
        self.0.route.set(Route::Session { session_id: target });
    }
}
