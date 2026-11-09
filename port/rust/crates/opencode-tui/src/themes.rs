//! `useThemes()` (`context/theme.tsx`'s `Themes` service): the selected theme name, the available themes, the mode
//! and its lock, `set(theme)` and `setMode(mode)` persisted to cli.json, with `tui().theme` following the
//! selection (the `selected` memo).
//!
//! TODO(port-tui): of the theme context, custom theme discovery, the `system` theme's palette probing and the
//! renderer's THEME_MODE events are not wired; the theme context belongs with the app shell.
use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_core::reactive::*;

use crate::config_context::ConfigContext;
use crate::theme::{Mode, ResolvedTheme, ThemeRegistry};

#[derive(Clone)]
pub struct Themes {
    registry: Rc<RefCell<ThemeRegistry>>,
    active: Signal<String>,
    mode: Signal<Mode>,
    lock: Signal<Option<Mode>>,
    renderer_mode: Mode,
    /// (name loaded, modes, mode).
    selected: Memo<(String, Vec<Mode>, Mode)>,
    config: ConfigContext,
}

thread_local! {
    static THEMES: RefCell<Option<Themes>> = const { RefCell::new(None) };
}

/// `useThemes()`.
pub fn use_themes() -> Option<Themes> {
    THEMES.with(|t| t.borrow().clone())
}

/// The theme context's init: the store from the config, and `theme` following the selection.
pub fn install(registry: ThemeRegistry, renderer_mode: Mode, config: ConfigContext, theme: Signal<ResolvedTheme>) -> Themes {
    let data = config.data_untracked();
    let config_mode = data.theme.as_ref().and_then(|t| t.mode.clone());
    let lock = config_mode.as_deref().and_then(Mode::parse);
    let active = create_signal(data.theme.as_ref().and_then(|t| t.name.clone()).unwrap_or_else(|| "opencode".into())).0;
    let mode = create_signal(lock.unwrap_or(renderer_mode)).0;
    let lock = create_signal(lock).0;
    let registry = Rc::new(RefCell::new(registry));
    let selected = {
        let registry = registry.clone();
        let config = config.clone();
        create_memo_equals(
            move || {
                let new_designs = config.data().new_designs();
                let name = active.get();
                let requested = mode.get();
                let result = registry.borrow_mut().select(&name, requested, new_designs);
                match result {
                    Ok(selected) => {
                        if selected.name != name {
                            active.set(selected.name.clone());
                        }
                        theme.set(selected.loaded.theme.clone());
                        (selected.name, selected.loaded.modes, selected.loaded.mode)
                    }
                    Err(_) => (name, vec![requested], requested),
                }
            },
            None,
        )
    };
    let themes = Themes { registry, active, mode, lock, renderer_mode, selected, config };
    THEMES.with(|t| *t.borrow_mut() = Some(themes.clone()));
    themes
}

/// `localeCompare(b, undefined, { sensitivity: "base" })` for theme names.
fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

impl Themes {
    /// `selected`: the active theme's name.
    pub fn selected(&self) -> String {
        self.active.get()
    }

    /// `Object.keys(themes.all())` sorted as the dialogs sort them.
    pub fn names(&self) -> Vec<String> {
        let new_designs = self.config.data().new_designs();
        let mut names: Vec<String> = self
            .registry
            .borrow_mut()
            .available(new_designs)
            .map(|all| all.into_iter().map(|(name, _)| name).collect())
            .unwrap_or_default();
        names.sort_by(|a, b| compare(a, b));
        names
    }

    pub fn has(&self, name: &str) -> bool {
        let new_designs = self.config.data_untracked().new_designs();
        self.registry.borrow_mut().has_available(name, new_designs)
    }

    /// `mode()`.
    pub fn mode(&self) -> Mode {
        self.selected.with(|s| s.2)
    }

    /// `modes()`.
    pub fn modes(&self) -> Vec<Mode> {
        self.selected.with(|s| s.1.clone())
    }

    pub fn supports(&self, mode: Mode) -> bool {
        self.modes().contains(&mode)
    }

    pub fn locked(&self) -> bool {
        self.lock.get().is_some()
    }

    fn persist_mode(&self, mode: &str) {
        let mode = mode.to_string();
        let _ = self.config.update(|draft| set_theme_field(draft, "mode", mode));
    }

    fn pin(&self, mode: Mode, persist: bool) {
        self.lock.set(Some(mode));
        self.mode.set(mode);
        if persist {
            self.persist_mode(mode.as_str());
        }
    }

    /// `lock()`.
    pub fn lock(&self) {
        self.pin(self.mode(), true);
    }

    /// `unlock()`.
    pub fn unlock(&self) {
        self.lock.set(None);
        self.mode.set(self.renderer_mode);
        self.persist_mode("system");
    }

    /// `setMode(mode, persist)`.
    pub fn set_mode(&self, mode: Mode, persist: bool) -> bool {
        if !self.supports(mode) {
            return false;
        }
        self.pin(mode, persist);
        true
    }

    /// `set(theme)`: false when the theme is not available or fails to load.
    pub fn set(&self, theme: &str) -> bool {
        if !self.has(theme) {
            return false;
        }
        self.active.set(theme.to_string());
        self.selected.get_untracked();
        if self.active.get_untracked() != theme {
            return false;
        }
        let name = theme.to_string();
        let _ = self.config.update(|draft| set_theme_field(draft, "name", name));
        true
    }
}

fn set_theme_field(draft: &mut opencode_protocol::Json, key: &str, value: String) {
    use opencode_protocol::Json;
    if !matches!(draft, Json::Object(_)) {
        *draft = Json::Object(Default::default());
    }
    let Json::Object(object) = draft else { return };
    let theme = object.entry("theme".to_string()).or_insert_with(|| Json::Object(Default::default()));
    if !matches!(theme, Json::Object(_)) {
        *theme = Json::Object(Default::default());
    }
    if let Json::Object(theme) = theme {
        theme.insert(key.to_string(), Json::String(value));
    }
}
