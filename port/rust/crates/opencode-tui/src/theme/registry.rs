//! The theme list and loading: `packages/tui/src/theme/index.ts` (built-in, custom and system themes, `parseTheme`)
//! and the non-reactive part of `packages/tui/src/context/theme.tsx` (`loadTheme`, the `selected` memo's fallback
//! to `opencode`, `has`).
//!
//! The TS keeps the list in module state and notifies subscribers (`subscribeThemes`); here a [`ThemeRegistry`] holds
//! it and the caller re-reads [`ThemeRegistry::all`] after a `set_*` call. Sources are `Rc`s, and parsed documents are
//! cached per source, as the TS caches them per source object (`WeakMap`).

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use opencode_protocol::Json;

use super::resolve::{ThemeError, parse_theme_document, resolve_theme_document};
use super::resolved::ResolvedTheme;
use super::select::{Mode, theme_modes};
use super::v1::{migrate_v1, new_designs::migrate_v1_new_designs};
use super::value::{Rec, Value};

/// A theme source: a V1 theme (`{ theme }`) or a theme document (`{ base, ... }`), as parsed JSON (or the generated
/// system theme).
pub type ThemeSource = Rc<Value>;

macro_rules! builtin {
    ($($name:literal => $file:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("assets/", $file)))),*]
    };
}

/// `DEFAULT_THEMES` (packages/tui/src/theme/v1.ts), in its order: name and JSON text.
pub const DEFAULT_THEMES: &[(&str, &str)] = builtin! {
    "aura" => "aura.json",
    "ayu" => "ayu.json",
    "catppuccin" => "catppuccin.json",
    "catppuccin-frappe" => "catppuccin-frappe.json",
    "catppuccin-macchiato" => "catppuccin-macchiato.json",
    "cobalt2" => "cobalt2.json",
    "cursor" => "cursor.json",
    "dracula" => "dracula.json",
    "everforest" => "everforest.json",
    "flexoki" => "flexoki.json",
    "github" => "github.json",
    "gruvbox" => "gruvbox.json",
    "kanagawa" => "kanagawa.json",
    "material" => "material.json",
    "matrix" => "matrix.json",
    "mercury" => "mercury.json",
    "monokai" => "monokai.json",
    "nightowl" => "nightowl.json",
    "nord" => "nord.json",
    "one-dark" => "one-dark.json",
    "osaka-jade" => "osaka-jade.json",
    "orng" => "orng.json",
    "lucent-orng" => "lucent-orng.json",
    "palenight" => "palenight.json",
    "rosepine" => "rosepine.json",
    "solarized" => "solarized.json",
    "synthwave84" => "synthwave84.json",
    "tokyonight" => "tokyonight.json",
    "vesper" => "vesper.json",
    "vercel" => "vercel.json",
    "zenburn" => "zenburn.json",
    "carbonfox" => "carbonfox.json",
};

/// The V1 `opencode.json` asset (not in `DEFAULT_THEMES`; the v2 document replaces it).
pub const OPENCODE_V1: &str = include_str!("assets/opencode.json");
/// `assets/v2/opencode.json`.
pub const OPENCODE: &str = include_str!("assets/v2/opencode.json");
/// `assets/v2/opencode2.json` (the "new-designs" experiment's theme).
pub const OPENCODE2: &str = include_str!("assets/v2/opencode2.json");

fn source(text: &str) -> ThemeSource {
    Rc::new(Value::from_json(&opencode_protocol::wire::json::parse(text).expect("built-in theme JSON")))
}

/// `isThemeSource(source)`: an object with `theme` or `base`.
pub fn is_theme_source(source: &Value) -> bool {
    source.as_rec().is_some_and(|r| r.get("theme").is_some() || r.get("base").is_some())
}

/// A theme loaded for a mode (`loadTheme` of context/theme.tsx).
#[derive(Clone, Debug)]
pub struct LoadedTheme {
    /// The modes the theme provides.
    pub modes: Vec<Mode>,
    /// The mode it was resolved in (the requested one when provided).
    pub mode: Mode,
    pub theme: ResolvedTheme,
}

/// The outcome of the `selected` memo of context/theme.tsx.
#[derive(Clone, Debug)]
pub struct Selected {
    pub loaded: LoadedTheme,
    /// The theme actually loaded (the requested one, or `opencode`).
    pub name: String,
    /// The theme that failed to load (`themeErrors.emit(name, error)`); `active` then becomes `opencode`.
    pub error: Option<(String, ThemeError)>,
}

/// The theme context's store as `ThemeProvider` initializes it (context/theme.tsx): `lock` from a configured
/// `dark`/`light` mode, `mode` from the lock, else the renderer's detected mode, else the provider's, `active` from
/// the configured name (default `opencode`).
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeState {
    pub mode: Mode,
    pub lock: Option<Mode>,
    pub active: String,
    pub ready: bool,
}

impl ThemeState {
    pub fn initial(config_mode: Option<&str>, config_name: Option<&str>, renderer_mode: Option<Mode>, props_mode: Mode) -> ThemeState {
        let lock = config_mode.and_then(Mode::parse);
        ThemeState {
            mode: lock.or(renderer_mode).unwrap_or(props_mode),
            lock,
            active: config_name.unwrap_or("opencode").to_owned(),
            ready: false,
        }
    }
}

/// The theme list (module state of theme/index.ts).
pub struct ThemeRegistry {
    defaults: Vec<(String, ThemeSource)>,
    opencode: ThemeSource,
    opencode2: Option<Result<ThemeSource, ThemeError>>,
    custom: Vec<(String, ThemeSource)>,
    system: Option<ThemeSource>,
    cache: RefCell<Vec<(Weak<Value>, bool, Rc<Rec>)>>,
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeRegistry {
    pub fn new() -> ThemeRegistry {
        let defaults = DEFAULT_THEMES.iter().map(|(name, text)| (name.to_string(), source(text))).collect();
        ThemeRegistry {
            defaults,
            opencode: Rc::new(Value::Rec(opencode_theme().clone())),
            opencode2: None,
            custom: Vec::new(),
            system: None,
            cache: RefCell::new(Vec::new()),
        }
    }

    /// `allThemes()` (`listThemes`): defaults < custom files < generated system, in JS property order.
    pub fn all(&self) -> Vec<(String, ThemeSource)> {
        let mut themes: Vec<(String, ThemeSource)> = Vec::new();
        for (name, value) in &self.defaults {
            put(&mut themes, name, value.clone());
        }
        put(&mut themes, "opencode", self.opencode.clone());
        for (name, value) in &self.custom {
            put(&mut themes, name, value.clone());
        }
        let system = self
            .system
            .clone()
            .or_else(|| themes.iter().find(|(n, _)| n == "system").map(|(_, v)| v.clone()))
            .unwrap_or_else(|| self.opencode.clone());
        put(&mut themes, "system", system);
        js_order(themes)
    }

    /// The `available` memo of the theme context: `{ opencode2, ...allThemes() }` while the "new-designs"
    /// experiment is on.
    pub fn available(&mut self, new_designs: bool) -> Result<Vec<(String, ThemeSource)>, ThemeError> {
        if !new_designs {
            return Ok(self.all());
        }
        let opencode2 = self.opencode2()?;
        let mut themes = vec![("opencode2".to_owned(), opencode2)];
        for (name, value) in self.all() {
            put(&mut themes, &name, value);
        }
        Ok(js_order(themes))
    }

    /// `getOpenCode2Theme()`.
    pub fn opencode2(&mut self) -> Result<ThemeSource, ThemeError> {
        self.opencode2
            .get_or_insert_with(|| parse_theme_document(&source(OPENCODE2), "opencode2").map(|d| Rc::new(Value::Rec(d))))
            .clone()
    }

    /// `hasTheme(name)`.
    pub fn has(&self, name: &str) -> bool {
        !name.is_empty() && self.all().iter().any(|(n, _)| n == name)
    }

    /// `setCustomThemes(themes)`: the discovered files that are theme sources.
    pub fn set_custom_themes(&mut self, themes: Vec<(String, Json)>) {
        let custom = themes
            .into_iter()
            .map(|(name, json)| (name, Value::from_json(&json)))
            .filter(|(_, value)| is_theme_source(value))
            .map(|(name, value)| (name, Rc::new(value)))
            .collect();
        self.custom = js_order(custom);
    }

    /// `setSystemTheme(theme)`.
    pub fn set_system_theme(&mut self, theme: Option<Value>) {
        self.system = theme.map(Rc::new);
    }

    /// `parseTheme(source, name, { newDesigns })`: a V1 theme migrated (no validation), or a document decoded.
    pub fn parse_theme(&self, source: &ThemeSource, name: &str, new_designs: bool) -> Result<Rc<Rec>, ThemeError> {
        {
            let mut cache = self.cache.borrow_mut();
            cache.retain(|(weak, _, _)| weak.strong_count() > 0);
            if let Some((_, _, document)) =
                cache.iter().find(|(weak, flag, _)| *flag == new_designs && std::ptr::eq(weak.as_ptr(), Rc::as_ptr(source)))
            {
                return Ok(document.clone());
            }
        }
        let document = match source.as_rec() {
            Some(rec) if rec.get("theme").is_some() => {
                if new_designs { migrate_v1_new_designs(rec)? } else { migrate_v1(rec)? }
            }
            _ => parse_theme_document(source, name)?,
        };
        let document = Rc::new(document);
        self.cache.borrow_mut().push((Rc::downgrade(source), new_designs, document.clone()));
        Ok(document)
    }

    /// `loadTheme(source, name, requested, newDesigns)`.
    pub fn load_theme(&self, source: &ThemeSource, name: &str, requested: Mode, new_designs: bool) -> Result<LoadedTheme, ThemeError> {
        let document = self.parse_theme(source, name, new_designs)?;
        let modes = theme_modes(&document);
        let mode = if modes.contains(&requested) { requested } else { modes.first().copied().unwrap_or(requested) };
        let theme = resolve_theme_document(&document, Some(mode))?;
        Ok(LoadedTheme { modes, mode, theme })
    }

    /// The `selected` memo: `active` if it is available (else `opencode`), falling back to `opencode` when it fails
    /// to load. Errors loading `opencode` itself are returned.
    pub fn select(&mut self, active: &str, mode: Mode, new_designs: bool) -> Result<Selected, ThemeError> {
        let sources = self.available(new_designs)?;
        let find = |name: &str| sources.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
        let (name, source) = match find(active) {
            Some(source) => (active.to_owned(), source),
            None => ("opencode".to_owned(), find("opencode").expect("opencode is always available")),
        };
        match self.load_theme(&source, &name, mode, new_designs) {
            Ok(loaded) => Ok(Selected { loaded, name, error: None }),
            Err(error) if name != "opencode" => {
                let fallback = find("opencode").expect("opencode is always available");
                let loaded = self.load_theme(&fallback, "opencode", mode, new_designs)?;
                Ok(Selected { loaded, name: "opencode".into(), error: Some((name, error)) })
            }
            Err(error) => Err(error),
        }
    }

    /// The theme context's `has(name)`: available in the current list.
    pub fn has_available(&mut self, name: &str, new_designs: bool) -> bool {
        !name.is_empty() && self.available(new_designs).is_ok_and(|all| all.iter().any(|(n, _)| n == name))
    }
}

/// `o[name] = value` on an entry list: an existing name keeps its position.
fn put(themes: &mut Vec<(String, ThemeSource)>, name: &str, value: ThemeSource) {
    match themes.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = value,
        None => themes.push((name.to_owned(), value)),
    }
}

/// Reorders entries as a JS object built from them iterates (array-index keys first, ascending).
fn js_order<T>(entries: Vec<(String, T)>) -> Vec<(String, T)> {
    let (mut index, rest): (Vec<_>, Vec<_>) = entries.into_iter().partition(|(k, _)| super::js::is_array_index(k));
    index.sort_by_key(|(k, _)| k.parse::<u64>().unwrap());
    index.extend(rest);
    index
}

/// `getOpenCodeTheme()`: the decoded v2 `opencode` document (it always decodes).
pub fn opencode_theme() -> &'static Rec {
    static OPENCODE_DOCUMENT: std::sync::LazyLock<Rec> = std::sync::LazyLock::new(|| {
        parse_theme_document(&source(OPENCODE), "opencode").expect("the built-in opencode theme decodes")
    });
    &OPENCODE_DOCUMENT
}
