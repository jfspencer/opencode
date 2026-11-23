//! `context/session-tabs.tsx` (`SessionTabsProvider` / `useSessionTabs`): the open session tabs and their scope,
//! history, recently closed stack, unread/attention status, per-tab scroll anchors and group disclosure, and the
//! viewed acknowledgement (`session.view`).
//!
//! The provider lives under the app's root owner (as in app.tsx, where it wraps `<App>`); [`install`] creates it and
//! [`tabs`] returns it. The store (`useStorage().store("tabs")`) is the same JSON file under
//! `<state>/<channel>/tui/tabs.json`; mutations apply in memory and are written atomically.
//!
//! TODO(port-tui): the storage file lock and on-disk draft (`Flock.withLock`, so a second TUI sharing the file sees
//! the same mutations), `createSessionRetention` (cache eviction) and `createProgramStatus` (OSC 7501 records).
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderer;
use opencode_tui_sync::client::RequestOptions;
use opencode_tui_sync::connection::Status;
use opencode_tui_sync::data::SessionStatus;
use opencode_tui_sync::json::JsonExt;
use opencode_tui_sync::obj;

use crate::ctx::{Route, Tui, tui};
use crate::data::{array, get, get_str};
use crate::session::anchors::{AnchorTarget, anchor_key};
use crate::tabs_model::{
    self, ClosedSessionTab, NEW_SESSION_TAB_TITLE, SessionTab, SessionTabHistory, Unread, close_session_tab,
    move_session_tab_history, open_session_tab, record_closed_session_tab, record_session_tab_history, reopen_session_tab,
};

/// A tab's unread/attention/busy state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabStatus {
    pub unread: Option<Unread>,
    pub prompt_pulse: u32,
    pub attention: Attention,
    pub busy: bool,
    pub renaming: bool,
}

/// What a tab needs input for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Attention {
    #[default]
    None,
    Permission,
    Question,
}

/// `ScrollAnchor`: where a tab's transcript sat.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollAnchor {
    pub target: AnchorTarget,
    pub screen_y: f64,
}

/// One scope's persisted tabs (`unread` stays empty for rollback compatibility, as in TS).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabsState {
    pub tabs: Vec<SessionTab>,
}

/// The persisted `tabs.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersistedState {
    pub global: TabsState,
    pub cwd: BTreeMap<String, TabsState>,
}

fn tab_of(value: &Json) -> Option<SessionTab> {
    Some(SessionTab::new(get_str(value, "sessionID")?.to_string(), get_str(value, "title").map(str::to_string)))
}

fn state_of(value: Option<&Json>) -> TabsState {
    TabsState {
        tabs: value.and_then(|v| get(v, "tabs")).map(array).unwrap_or_default().iter().filter_map(tab_of).collect(),
    }
}

fn parse_state(value: &Json) -> PersistedState {
    let mut persisted = PersistedState { global: state_of(Some(value)), cwd: BTreeMap::new() };
    if let Some(Json::Object(cwd)) = get(value, "cwd") {
        for (key, value) in cwd {
            persisted.cwd.insert(key.clone(), state_of(Some(value)));
        }
    }
    persisted
}

fn state_json(state: &TabsState) -> Json {
    let tabs = state
        .tabs
        .iter()
        .map(|tab| match &tab.title {
            Some(title) => obj! { "sessionID" => tab.session_id.as_str(), "title" => title.as_str() },
            None => obj! { "sessionID" => tab.session_id.as_str() },
        })
        .collect();
    obj! { "tabs" => Json::Array(tabs), "unread" => obj! {} }
}

fn persisted_json(persisted: &PersistedState) -> Json {
    let cwd: opencode_protocol::wire::Object =
        persisted.cwd.iter().map(|(key, state)| (key.clone(), state_json(state))).collect();
    obj! { "global" => state_json(&persisted.global), "cwd" => Json::Object(cwd) }
}

struct Inner {
    t: Tui,
    path: PathBuf,
    persisted: Signal<PersistedState>,
    /// `createSignal<boolean>()`: undefined until a key or a terminal focus report.
    focused: Signal<Option<bool>>,
    prompt_pulses: Signal<HashMap<String, u32>>,
    history: RefCell<SessionTabHistory>,
    closed: Signal<Vec<ClosedSessionTab>>,
    cancelled: RefCell<HashSet<String>>,
    anchors: RefCell<HashMap<String, ScrollAnchor>>,
    expanded: Signal<HashMap<String, HashMap<String, bool>>>,
    new_tab: Memo<bool>,
    view_retry: Signal<u32>,
    acknowledged: RefCell<HashMap<String, f64>>,
    view_retry_timer: Cell<Option<u64>>,
    view_retry_attempt: Cell<u32>,
}

/// `useSessionTabs()`.
#[derive(Clone)]
pub struct SessionTabs(Rc<Inner>);

thread_local! {
    static TABS: RefCell<Option<SessionTabs>> = const { RefCell::new(None) };
}

/// `useSessionTabs()`.
pub fn tabs() -> SessionTabs {
    TABS.with(|t| t.borrow().clone()).unwrap_or_else(install)
}

impl SessionTabs {
    /// `enabled`: `config.tabs.enabled`, tracked.
    pub fn enabled(&self) -> bool {
        self.0.t.config_context.data().tabs.enabled
    }

    fn enabled_untracked(&self) -> bool {
        self.0.t.config_context.data_untracked().tabs.enabled
    }

    fn scope(&self) -> String {
        self.0.t.config_context.data_untracked().tabs.scope.clone()
    }

    fn cwd(&self) -> String {
        self.0.t.paths.cwd.clone()
    }

    /// `state()`.
    fn state(&self) -> TabsState {
        let scope = self.scope();
        self.0.persisted.with(|persisted| {
            if scope == "cwd" {
                persisted.cwd.get(&self.cwd()).cloned().unwrap_or_default()
            } else {
                persisted.global.clone()
            }
        })
    }

    /// `state()`, untracked.
    fn state_untracked(&self) -> TabsState {
        let scope = self.scope();
        self.0.persisted.with_untracked(|persisted| {
            if scope == "cwd" {
                persisted.cwd.get(&self.cwd()).cloned().unwrap_or_default()
            } else {
                persisted.global.clone()
            }
        })
    }

    /// `update(mutation)`: mutate the scope's draft, publish it and write the file.
    fn update(&self, mutation: impl FnOnce(&mut TabsState)) {
        let scope = self.scope();
        let cwd = self.cwd();
        let next = self.0.persisted.with_untracked(|persisted| {
            let mut next = persisted.clone();
            let target = if scope == "cwd" { next.cwd.entry(cwd).or_default() } else { &mut next.global };
            mutation(target);
            next
        });
        self.0.persisted.set(next.clone());
        self.save(next);
    }

    fn save(&self, persisted: PersistedState) {
        let path = self.0.path.clone();
        let value = persisted_json(&persisted);
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

    fn session(&self, id: &str) -> Option<Rc<Json>> {
        self.0.t.data.store.session.get(id)
    }

    fn root(&self, id: &str) -> String {
        self.0.t.data.store.session.root(id)
    }

    /// `title(sessionID, persisted?, fallback?)`.
    fn title(&self, session_id: &str, persisted: Option<&str>, fallback: Option<&str>) -> Option<String> {
        let session = self.session(session_id);
        session
            .as_ref()
            .and_then(|session| session.str("title").map(str::to_string))
            .or_else(|| persisted.map(str::to_string))
            .or_else(|| fallback.map(str::to_string))
            .or_else(|| session.as_deref().map(crate::subagents::with_timestamped_fallback))
    }

    /// `isUnread(sessionID)`.
    fn is_unread(&self, session_id: &str) -> bool {
        let Some(info) = self.session(session_id) else { return false };
        let Some(time) = info.get("time") else { return false };
        let idle = time.num("idle");
        idle.is_some_and(|idle| match time.num("viewed") {
            None => true,
            Some(viewed) => idle > viewed,
        })
    }

    /// `family(sessionID)`.
    fn family(&self, session_id: &str) -> Vec<String> {
        let session = self.root(session_id);
        let members: Vec<String> = self.0.t.data.store.session.family(&session).to_vec();
        if members.contains(&session) {
            return members;
        }
        std::iter::once(session.clone()).chain(members).collect()
    }

    /// `current()`.
    pub fn current(&self) -> Option<String> {
        match self.0.t.route.get() {
            Route::Session { session_id } => Some(self.root(&session_id)),
            Route::Home => None,
        }
    }

    /// `newTab()`: home, or the latched new-session slot until the current session enters the tabs.
    pub fn new_tab(&self) -> bool {
        self.0.new_tab.get()
    }

    /// `tabs()`.
    pub fn list(&self) -> Vec<SessionTab> {
        self.state().tabs
    }

    /// `recentlyClosed()`.
    pub fn recently_closed(&self) -> Vec<SessionTab> {
        let open = self.state_untracked().tabs;
        self.0
            .closed
            .get()
            .into_iter()
            .rev()
            .filter(|entry| !open.iter().any(|tab| tab.session_id == entry.tab.session_id))
            .map(|entry| {
                let title = self.session(&entry.tab.session_id).and_then(|s| s.str("title").map(str::to_string));
                SessionTab { session_id: entry.tab.session_id.clone(), title: title.or(entry.tab.title.clone()) }
            })
            .collect()
    }

    /// `status(sessionID)`.
    pub fn status(&self, session_id: &str) -> TabStatus {
        let session = self.root(session_id);
        let members = self.family(&session);
        let info = self.session(&session);
        let unread = if !self.is_unread(&session) {
            None
        } else if info.as_ref().and_then(|info| info.str("outcome")) == Some("failed") {
            Some(Unread::Error)
        } else {
            Some(Unread::Activity)
        };
        let has_requests = |id: &str| self.0.t.data.store.session.permission.list(id).is_some_and(|list| !list.is_empty());
        let has_forms = |id: &str| self.0.t.data.store.session.form.list(id, None).is_some_and(|list| !list.is_empty());
        let attention = if members.iter().any(|id| has_requests(id)) {
            Attention::Permission
        } else if members.iter().any(|id| has_forms(id)) {
            Attention::Question
        } else {
            Attention::None
        };
        let busy = members.iter().any(|id| {
            self.0.t.data.store.session.status(id) == SessionStatus::Running
                || self.0.t.data.store.session.pending.list(id).iter().any(|item| item.str("type") != Some("synthetic"))
        });
        TabStatus {
            unread,
            prompt_pulse: self.0.prompt_pulses.with(|pulses| pulses.get(&session).copied().unwrap_or(0)),
            attention,
            busy,
            renaming: self.0.t.data.store.session.title.pending(&session),
        }
    }

    /// `scrollAnchor(sessionID)`.
    pub fn scroll_anchor(&self, session_id: &str) -> Option<ScrollAnchor> {
        let target = self.root(session_id);
        if !self.state_untracked().tabs.iter().any(|tab| tab.session_id == target) {
            return None;
        }
        self.0.anchors.borrow().get(session_id).cloned()
    }

    /// `setScrollAnchor(sessionID, anchor)`.
    pub fn set_scroll_anchor(&self, session_id: &str, anchor: Option<ScrollAnchor>) {
        let target = self.root(session_id);
        let open = self.state_untracked().tabs.iter().any(|tab| tab.session_id == target);
        match anchor {
            Some(anchor) if open => {
                if let Some(current) = self.0.anchors.borrow().get(session_id) {
                    if anchor_key(&current.target) == anchor_key(&anchor.target) && current.screen_y == anchor.screen_y {
                        return;
                    }
                }
                self.0.anchors.borrow_mut().insert(session_id.to_string(), anchor);
            }
            _ => {
                self.0.anchors.borrow_mut().remove(session_id);
            }
        }
    }

    /// `groupExpanded(sessionID, groupID)`.
    pub fn group_expanded(&self, session_id: &str, group_id: &str) -> Option<bool> {
        self.0.expanded.with(|map| map.get(session_id).and_then(|groups| groups.get(group_id).copied()))
    }

    /// `setGroupExpanded(sessionID, groupID, expanded)`.
    pub fn set_group_expanded(&self, session_id: &str, group_id: &str, expanded: bool) {
        self.0.expanded.update(|map| {
            map.entry(session_id.to_string()).or_default().insert(group_id.to_string(), expanded);
        });
    }

    /// `select(sessionID)`.
    pub fn select(&self, session_id: &str) {
        if !self.enabled_untracked() {
            return;
        }
        self.0.t.route.set(Route::Session { session_id: self.root(session_id) });
    }

    /// `open(sessionID)`.
    pub fn open(&self, session_id: &str) {
        if !self.enabled_untracked() {
            return;
        }
        let session = self.root(session_id);
        if self.state_untracked().tabs.iter().any(|tab| tab.session_id == session) {
            return;
        }
        self.0.cancelled.borrow_mut().remove(&session);
        let title = self.title(&session, None, None);
        self.update(|draft| {
            draft.tabs = open_session_tab(&draft.tabs, SessionTab::new(session.clone(), title));
        });
    }

    /// `add()`: a new session, as the home screen's "New session" tab.
    pub fn add(&self) {
        if !self.enabled_untracked() {
            return;
        }
        // TODO(port-tui): the home route's `location` (`newSessionLocation`); the Rust route carries none yet.
        self.0.t.route.set(Route::Home);
    }

    /// `close(sessionID?)`.
    pub fn close(&self, session_id: Option<&str>) {
        if !self.enabled_untracked() {
            return;
        }
        let target = session_id.map(|id| self.root(id)).or_else(|| self.current());
        let Some(target) = target else {
            let (history, session) = move_session_tab_history(&self.0.history.borrow(), &self.state_untracked().tabs, None, -1);
            *self.0.history.borrow_mut() = history;
            let session = session.or_else(|| self.state_untracked().tabs.last().map(|tab| tab.session_id.clone()));
            if matches!(self.0.t.route.get_untracked(), Route::Home) {
                if let Some(session) = session {
                    self.0.t.route.set(Route::Session { session_id: session });
                }
            }
            return;
        };
        let tabs = self.state_untracked().tabs;
        if let Some(index) = tabs.iter().position(|tab| tab.session_id == target) {
            self.0.closed.set(record_closed_session_tab(&self.0.closed.get_untracked(), tabs[index].clone(), index));
        }
        self.remove(&target, true);
    }

    /// `reopen(sessionID?)`.
    pub fn reopen(&self, session_id: Option<&str>) {
        if !self.enabled_untracked() {
            return;
        }
        let result = reopen_session_tab(&self.0.closed.get_untracked(), &self.state_untracked().tabs, session_id);
        self.0.closed.set(result.stack);
        let (Some(tabs), Some(session)) = (result.tabs, result.session_id) else { return };
        self.0.cancelled.borrow_mut().remove(&session);
        self.update(|draft| {
            draft.tabs = tabs;
        });
        self.0.t.route.set(Route::Session { session_id: session });
    }

    /// `move(sessionID, index)`.
    pub fn move_tab(&self, session_id: &str, index: i64) {
        if !self.enabled_untracked() {
            return;
        }
        let session = self.root(session_id);
        if tabs_model::move_session_tab(&self.state_untracked().tabs, &session, index) == self.state_untracked().tabs {
            return;
        }
        self.update(|draft| {
            draft.tabs = tabs_model::move_session_tab(&draft.tabs, &session, index);
        });
    }

    /// `cycle(direction)`.
    pub fn cycle(&self, direction: i64) {
        if !self.enabled_untracked() {
            return;
        }
        let active = self.current();
        if let Some(tab) = tabs_model::cycle_session_tab(&self.state_untracked().tabs, active.as_deref(), direction, |_| true) {
            self.0.t.route.set(Route::Session { session_id: tab.session_id });
        }
    }

    /// `cycleUnread(direction)`.
    pub fn cycle_unread(&self, direction: i64) {
        if !self.enabled_untracked() {
            return;
        }
        let active = self.current();
        let statuses = self
            .state_untracked()
            .tabs
            .iter()
            .map(|tab| (tab.session_id.clone(), self.status(&tab.session_id)))
            .collect::<HashMap<_, _>>();
        let matches = |tab: &SessionTab| {
            statuses
                .get(&tab.session_id)
                .is_some_and(|status| status.unread.is_some() || status.attention != Attention::None)
        };
        if let Some(tab) = tabs_model::cycle_session_tab(&self.state_untracked().tabs, active.as_deref(), direction, matches) {
            self.0.t.route.set(Route::Session { session_id: tab.session_id });
        }
    }

    /// `history(direction)`.
    pub fn history(&self, direction: i64) {
        if !self.enabled_untracked() {
            return;
        }
        let active = self.current();
        let (history, session) =
            move_session_tab_history(&self.0.history.borrow(), &self.state_untracked().tabs, active.as_deref(), direction);
        *self.0.history.borrow_mut() = history;
        if let Some(session) = session {
            self.0.t.route.set(Route::Session { session_id: session });
        }
    }

    /// `selectIndex(index)`.
    pub fn select_index(&self, index: usize) {
        if !self.enabled_untracked() {
            return;
        }
        if let Some(tab) = self.state_untracked().tabs.get(index) {
            self.0.t.route.set(Route::Session { session_id: tab.session_id.clone() });
        }
    }

    /// `remove(sessionID, navigate)`.
    fn remove(&self, session_id: &str, navigate: bool) {
        let target = self.root(session_id);
        self.0.cancelled.borrow_mut().insert(target.clone());
        for id in self.family(&target) {
            self.0.anchors.borrow_mut().remove(&id);
            self.0.expanded.update(|map| {
                map.remove(&id);
            });
        }
        let known = self.current_tab();
        let tabs = self.state_untracked().tabs;
        let last_index = known.as_ref().filter(|(known, _)| known == &target).map(|(_, index)| *index);
        let closed = close_session_tab(&tabs, &target, last_index);
        let selected = navigate && self.current().as_deref() == Some(target.as_str());
        if !closed.changed && !selected {
            return;
        }
        let (history, previous) = if selected {
            let recorded = record_session_tab_history(&self.0.history.borrow(), &target);
            let (history, previous) = move_session_tab_history(&recorded, &closed.tabs, Some(&target), -1);
            (history, previous)
        } else {
            (self.0.history.borrow().clone(), None)
        };
        let next = previous.or(closed.next);
        *self.0.history.borrow_mut() = history;
        self.update(|draft| {
            draft.tabs = close_session_tab(&draft.tabs, &target, None).tabs;
        });
        self.0.prompt_pulses.update(|pulses| {
            pulses.remove(&target);
        });
        if selected {
            self.0.t.route.set(match next {
                Some(next) => Route::Session { session_id: next },
                None => Route::Home,
            });
        }
    }

    /// `currentTab`: the viewed session and where its tab last sat.
    fn current_tab(&self) -> Option<(String, usize)> {
        let session = self.current()?;
        let tabs = self.state_untracked().tabs;
        let index = tabs.iter().position(|tab| tab.session_id == session)?;
        Some((session, index))
    }

    /// `normalize(value)`.
    fn normalize(&self, value: &TabsState) -> TabsState {
        let tabs = value.tabs.iter().fold(Vec::new(), |tabs, tab| {
            let session = self.root(&tab.session_id);
            let title = self.title(&session, tab.title.as_deref(), None);
            open_session_tab(&tabs, SessionTab::new(session, title))
        });
        TabsState { tabs }
    }

    fn prompt_pulse(&self, session_id: &str) {
        self.0.prompt_pulses.update(|pulses| {
            *pulses.entry(session_id.to_string()).or_insert(0) += 1;
        });
    }
}

/// From the component tree: `useSessionTabs()`.
pub fn use_tabs() -> SessionTabs {
    tabs()
}

fn load(path: &PathBuf) -> PersistedState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| opencode_protocol::wire::json::parse(&text).ok())
        .map(|value| parse_state(&value))
        .unwrap_or_default()
}

/// `SessionTabsProvider`'s init, under the current owner.
pub fn install() -> SessionTabs {
    let t = tui();
    let path = PathBuf::from(&t.paths.state).join(&t.app.channel).join("tui").join("tabs.json");
    let persisted = create_signal_with(load(&path), None).0;
    let focused: Signal<Option<bool>> = create_signal(None::<bool>).0;
    let prompt_pulses: Signal<HashMap<String, u32>> = create_signal(HashMap::new()).0;
    let closed: Signal<Vec<ClosedSessionTab>> = create_signal(Vec::new()).0;
    let expanded: Signal<HashMap<String, HashMap<String, bool>>> = create_signal(HashMap::new()).0;
    let view_retry = create_signal(0u32).0;
    // `newTab`, a latching memo: home promotes it, and only a tab for the current session retires it.
    let new_tab = {
        let t = t.clone();
        let persisted = persisted;
        create_memo_with(move |previous: Option<bool>| {
            if matches!(t.route.get(), Route::Home) {
                return true;
            }
            if !previous.unwrap_or(false) {
                return false;
            }
            let session = match t.route.get() {
                Route::Session { session_id } => {
                    let root = t.data.store.session.root(&session_id);
                    Some(root)
                }
                Route::Home => None,
            };
            let scope = t.config_context.data_untracked().tabs.scope.clone();
            let cwd = t.paths.cwd.clone();
            let tabs = persisted.with(|persisted| {
                if scope == "cwd" {
                    persisted.cwd.get(&cwd).cloned().unwrap_or_default().tabs
                } else {
                    persisted.global.tabs.clone()
                }
            });
            session.is_some_and(|session| !tabs.iter().any(|tab| tab.session_id == session))
        })
    };
    let inner = Rc::new(Inner {
        t: t.clone(),
        path,
        persisted,
        focused,
        prompt_pulses,
        history: RefCell::new(SessionTabHistory::default()),
        closed,
        cancelled: RefCell::new(HashSet::new()),
        anchors: RefCell::new(HashMap::new()),
        expanded,
        new_tab,
        view_retry,
        acknowledged: RefCell::new(HashMap::new()),
        view_retry_timer: Cell::new(None),
        view_retry_attempt: Cell::new(0),
    });
    let tabs = SessionTabs(inner);
    TABS.with(|slot| *slot.borrow_mut() = Some(tabs.clone()));

    // `useKeyboard(onFocus)` and the renderer's focus/blur events.
    {
        let focused = tabs.0.focused;
        let off = crate::keymap::intercept_key(200, move |event| {
            // `useKeyboard` in the renderer sees the keys the focused renderable lets through: typing (a printable
            // key without modifiers) focuses the terminal; shortcuts, escape and backspace do not.
            let printable = (event.name().chars().count() == 1 || event.name() == "space")
                && !event.ctrl()
                && !event.meta()
                && !event.super_key()
                && !event.hyper();
            if printable {
                focused.set(Some(true));
            }
            false
        });
        on_cleanup(off);
        let focused = tabs.0.focused;
        let token = renderer::on_terminal_focus(move |focused_now| focused.set(Some(focused_now)));
        on_cleanup(move || renderer::off_terminal_focus(token));
    }

    // Shared storage updates must not re-admit a tab unless this client changes route or scope.
    {
        let tabs = tabs.clone();
        create_effect(on(
            {
                let tabs = tabs.clone();
                move || {
                    let enabled = tabs.enabled();
                    let scope = tabs.0.t.config_context.data().tabs.scope.clone();
                    let routed = match tabs.0.t.route.get() {
                        Route::Session { session_id } if enabled => Some(session_id),
                        _ => None,
                    };
                    (routed, scope)
                }
            },
            move |(routed, _), _| {
                let Some(routed) = routed.clone() else { return };
                if routed == "dummy" {
                    return;
                }
                let session = tabs.root(&routed);
                tabs.0.cancelled.borrow_mut().remove(&session);
                let history = record_session_tab_history(&tabs.0.history.borrow(), &session);
                *tabs.0.history.borrow_mut() = history;
                if tabs.state_untracked().tabs.iter().any(|tab| tab.session_id == session) {
                    return;
                }
                let fallback = tabs.new_tab().then_some(NEW_SESSION_TAB_TITLE);
                tabs.update(|draft| {
                    if tabs.0.cancelled.borrow().contains(&session) {
                        return;
                    }
                    let persisted = draft.tabs.iter().find(|tab| tab.session_id == session).and_then(|tab| tab.title.clone());
                    let title = tabs.title(&session, persisted.as_deref(), fallback);
                    draft.tabs = open_session_tab(&draft.tabs, SessionTab::new(session.clone(), title));
                });
            },
            false,
        ));
    }

    // Viewed state is server-global, so acknowledgement runs even with tabs disabled.
    {
        let tabs = tabs.clone();
        let retry = tabs.0.view_retry;
        let effect_tabs = tabs.clone();
        create_effect(move || {
            let tabs = &effect_tabs;
            retry.get();
            if tabs.0.focused.get() != Some(true) {
                return;
            }
            let session = match tabs.0.t.route.get() {
                Route::Session { session_id } if session_id != "dummy" => session_id,
                _ => return,
            };
            let session_id = tabs.root(&session);
            let Some(idle) = tabs.session(&session_id).and_then(|info| info.at("time.idle").and_then(|i| match i {
                Json::Number(n) => Some(*n),
                _ => None,
            })) else {
                return;
            };
            if !tabs.is_unread(&session_id) {
                return;
            }
            if tabs.0.acknowledged.borrow().get(&session_id).copied() == Some(idle) {
                return;
            }
            // Record before the request so event-driven re-runs don't re-post the same watermark.
            tabs.0.acknowledged.borrow_mut().insert(session_id.clone(), idle);
            let call = tabs.0.t.data.client.call_named(
                "session.view",
                &obj! { "sessionID" => session_id.as_str(), "idle" => idle },
                RequestOptions::default(),
            );
            let this = tabs.clone();
            tokio::task::spawn_local(async move {
                match call.await {
                    Ok(_) => {
                        if let Some(timer) = this.0.view_retry_timer.take() {
                            renderer::clear_timeout(timer);
                        }
                        this.0.view_retry_attempt.set(0);
                    }
                    Err(_) => {
                        if this.0.acknowledged.borrow().get(&session_id).copied() != Some(idle) {
                            return;
                        }
                        this.0.acknowledged.borrow_mut().remove(&session_id);
                        if this.0.view_retry_timer.get().is_some() {
                            return;
                        }
                        let delay = (250u64 << this.0.view_retry_attempt.get().min(5)).min(5_000);
                        this.0.view_retry_attempt.set(this.0.view_retry_attempt.get() + 1);
                        let retry = this.0.view_retry;
                        let this2 = this.clone();
                        let timer = renderer::set_timeout(delay, move || {
                            this2.0.view_retry_timer.set(None);
                            retry.set_with(|value| value + 1);
                        });
                        this.0.view_retry_timer.set(Some(timer));
                    }
                }
            });
        });
        let inner = tabs.0.clone();
        on_cleanup(move || {
            if let Some(timer) = inner.view_retry_timer.take() {
                renderer::clear_timeout(timer);
            }
        });
    }

    // Normalize the stored tabs against the live sessions whenever one of them changes.
    {
        let tabs = tabs.clone();
        create_effect(move || {
            if !tabs.enabled() {
                return;
            }
            let current = tabs.state();
            let next = tabs.normalize(&current);
            if next == current {
                return;
            }
            tabs.update(|draft| {
                let next = tabs.normalize(draft);
                draft.tabs = next.tabs;
            });
        });
    }

    // Load tab metadata: the visible session keeps the first connection slots, then each background tab's data.
    {
        let tabs = tabs.clone();
        create_effect(move || {
            if !tabs.enabled() {
                return;
            }
            if tabs.0.t.data.connection.status() != Status::Connected {
                return;
            }
            let mut session_ids: Vec<String> = tabs.state().tabs.iter().map(|tab| tab.session_id.clone()).collect();
            if session_ids.is_empty() {
                return;
            }
            session_ids.sort();
            let this = tabs.clone();
            let sorted = session_ids.clone();
            let stale = Rc::new(Cell::new(false));
            let stale_for_task = stale.clone();
            tokio::task::spawn_local(async move {
                for id in &sorted {
                    let _ = this.0.t.data.store.session.sync(id, true).wait().await;
                }
                if stale_for_task.get() {
                    return;
                }
                let mut locations: Vec<Json> = Vec::new();
                let mut keys = HashSet::new();
                for id in &sorted {
                    let Some(location) = this.0.t.data.store.session.get(id).and_then(|s| s.get("location").cloned()) else { continue };
                    if keys.insert(opencode_tui_sync::data::location_key(&location)) {
                        locations.push(location);
                    }
                }
                for location in locations {
                    let _ = this.0.t.data.store.location.sync_info(Some(&location)).wait().await;
                    let _ = this.0.t.data.store.location.vcs.sync(Some(&location)).wait().await;
                }
            });
            let current = tabs.current();
            let session_ids = session_ids.clone();
            let stale_timer = stale.clone();
            let this = tabs.clone();
            let timer = renderer::set_timeout(300, move || {
                let this = this.clone();
                tokio::task::spawn_local(async move {
                    for id in session_ids {
                        if stale_timer.get() || current.as_deref() == Some(id.as_str()) {
                            continue;
                        }
                        let _ = this.0.t.data.store.session.message.sync(&id).wait().await;
                        let _ = this.0.t.data.store.session.pending.sync(&id).wait().await;
                        let _ = this.0.t.data.store.session.permission.sync(&id).wait().await;
                        let _ = this.0.t.data.store.session.form.sync(&id, None).wait().await;
                    }
                });
            });
            on_cleanup(move || {
                stale.set(true);
                renderer::clear_timeout(timer);
            });
        });
    }

    // Server events.
    {
        let event = tabs.0.t.data.event.clone();
        let moved = {
            let tabs = tabs.clone();
            event.on("session.moved", move |evt, _| {
                if !tabs.enabled_untracked() {
                    return;
                }
                let Some(session_id) = get_str(evt.data(), "sessionID").map(str::to_string) else { return };
                let root = tabs.root(&session_id);
                if !tabs.state_untracked().tabs.iter().any(|tab| tab.session_id == root) {
                    return;
                }
                let Some(location) = get(evt.data(), "location").cloned() else { return };
                let data = tabs.0.t.data.clone();
                tokio::task::spawn_local(async move {
                    let _ = data.store.location.sync_info(Some(&location)).wait().await;
                    let _ = data.store.location.vcs.sync(Some(&location)).wait().await;
                });
            })
        };
        let enqueued = {
            let tabs = tabs.clone();
            event.on("session.inbox.enqueued", move |evt, _| {
                if !tabs.enabled_untracked() {
                    return;
                }
                let Some(item_type) = get(evt.data(), "item").and_then(|item| get_str(item, "type")) else { return };
                if item_type != "user" {
                    return;
                }
                let Some(session_id) = get_str(evt.data(), "sessionID").map(str::to_string) else { return };
                let session_id = tabs.root(&session_id);
                if tabs.current().as_deref() == Some(session_id.as_str())
                    || !tabs.state_untracked().tabs.iter().any(|tab| tab.session_id == session_id)
                {
                    return;
                }
                tabs.prompt_pulse(&session_id);
            })
        };
        let deleted = {
            let tabs = tabs.clone();
            event.on("session.deleted", move |evt, _| {
                let Some(session_id) = get_str(evt.data(), "sessionID").map(str::to_string) else { return };
                let target = tabs.root(&session_id);
                tabs.0.closed.update(|entries| {
                    entries.retain(|entry| entry.tab.session_id != target);
                });
                tabs.remove(&session_id, tabs.enabled_untracked());
            })
        };
        on_cleanup(move || {
            moved.call();
            enqueued.call();
            deleted.call();
        });
    }

    tabs
}
