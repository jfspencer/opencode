//! `Config.resolve(info, options)` (packages/tui/src/config/index.tsx): the TUI config with every default applied,
//! as a struct the app reads (`config.data.mouse` is `config.mouse`, `config.data.session.new_location` is
//! `config.session.new_location`). Optional settings TS leaves `undefined` are `None`; literal settings are their
//! strings. [`Resolved::info`] keeps the decoded `Info` for anything read generically.

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

use super::keybind::{self, BindingLookup};

/// `Config.resolve`'s options.
#[derive(Clone, Debug)]
pub struct ResolveOptions {
    /// `process.platform !== "win32"`: always true on the platforms the port runs on.
    pub terminal_suspend: bool,
    /// `HERDR_ENV` of the environment (`options.environment ?? process.env`).
    pub herdr_env: Option<String>,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        ResolveOptions { terminal_suspend: true, herdr_env: std::env::var("HERDR_ENV").ok() }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeConfig {
    pub name: Option<String>,
    /// `system`, `dark` or `light`.
    pub mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attention {
    pub notifications: bool,
    pub sound: bool,
    pub volume: f64,
    pub sound_pack: String,
    /// Sound file overrides by attention event, in event order.
    pub sounds: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Leader {
    pub timeout: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    /// `block`, `underline`, `line` or `default`.
    pub style: String,
    pub blinking: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scroll {
    pub speed: Option<f64>,
    pub acceleration: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diffs {
    pub source: Option<String>,
    pub wrap: Option<String>,
    pub tree: Option<bool>,
    pub single: Option<bool>,
    pub fullscreen: Option<bool>,
    pub view: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Terminal {
    pub title: Option<bool>,
    /// `manual` or `select`.
    pub copy: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    pub editor: Option<bool>,
    /// `compact` or `full`.
    pub paste: Option<String>,
    pub image_preview: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub sidebar: Option<String>,
    pub scrollbar: Option<bool>,
    pub submit_scroll: Option<String>,
    pub thinking: Option<String>,
    pub grouping: Option<String>,
    pub verbosity: Option<String>,
    pub image_preview: Option<bool>,
    pub tps: bool,
    pub markdown: Option<String>,
    /// `launch` or `inherit`.
    pub new_location: String,
    /// `prompt` or `autoaccept`.
    pub permissions: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tabs {
    /// `auto`, `on` or `off`.
    pub mode: String,
    pub enabled: bool,
    /// `global` or `cwd`.
    pub scope: String,
    /// `horizontal` or `vertical`.
    pub layout: String,
    /// `status` or `numbers`.
    pub indicators: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Debug {
    pub devtools: Option<bool>,
    pub timing: Option<bool>,
    /// `true`, `false` or `"verbose"`.
    pub turn_tokens: Option<Json>,
}

/// `Config.Resolved`.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The decoded `Info` this was resolved from.
    pub info: Json,
    pub theme: Option<ThemeConfig>,
    pub keybinds: BindingLookup,
    /// Ordered plugin directives (strings or `{ package, options }`).
    pub plugins: Option<Vec<Json>>,
    pub leader: Leader,
    pub scroll: Option<Scroll>,
    pub attention: Attention,
    pub diffs: Option<Diffs>,
    pub terminal: Option<Terminal>,
    pub prompt: Option<Prompt>,
    pub session: Session,
    pub tabs: Tabs,
    /// The Mini settings (`mini/` is not ported).
    pub mini: Option<Json>,
    pub debug: Option<Debug>,
    pub experimental: Option<Vec<(String, bool)>>,
    pub animations: Option<bool>,
    pub highlights: Option<bool>,
    pub mouse: bool,
    pub cursor: Cursor,
}

fn group<'a>(info: &'a Object, key: &str) -> Option<&'a Object> {
    match info.get(key) {
        Some(Json::Object(o)) => Some(o),
        _ => None,
    }
}

fn str_at(object: Option<&Object>, key: &str) -> Option<String> {
    match object?.get(key) {
        Some(Json::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn bool_at(object: Option<&Object>, key: &str) -> Option<bool> {
    match object?.get(key) {
        Some(Json::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn num_at(object: Option<&Object>, key: &str) -> Option<f64> {
    match object?.get(key) {
        Some(Json::Number(n)) => Some(*n),
        _ => None,
    }
}

impl Resolved {
    /// `useNewDesigns()`: the "new-designs" experiment.
    pub fn new_designs(&self) -> bool {
        self.experiment("new-designs")
    }

    /// `useClientMcpExperiment()`: the "client-mcp" experiment.
    pub fn client_mcp(&self) -> bool {
        self.experiment("client-mcp")
    }

    /// `config.data.experimental?.[id] === true`.
    pub fn experiment(&self, id: &str) -> bool {
        self.experimental.as_ref().is_some_and(|e| e.iter().any(|(k, v)| k == id && *v))
    }
}

/// `Config.resolve(input, options)`. Fails as TS throws: an unrecognized keybind name, or a binding value the
/// schema rejects (neither happens for a decoded config).
pub fn resolve(input: &Json, options: &ResolveOptions) -> Result<Resolved, String> {
    let empty = Object::new();
    let info = match input {
        Json::Object(o) => o,
        _ => &empty,
    };
    let tabs = group(info, "tabs");
    let tabs_mode = str_at(tabs, "mode").unwrap_or_else(|| match bool_at(tabs, "enabled") {
        None => "auto".into(),
        Some(true) => "on".into(),
        Some(false) => "off".into(),
    });
    let mut keybinds: Object = group(info, "keybinds").cloned().unwrap_or_default();
    if !options.terminal_suspend {
        keybinds.insert("terminal.suspend".into(), Json::String("none".into()));
        if !keybinds.contains_key("input.undo") {
            let mut values = vec!["ctrl+z".to_owned()];
            if let Some(Json::String(default)) = keybind::default_value("input.undo") {
                values.extend(default.split(',').map(str::to_owned));
            }
            let mut unique: Vec<String> = Vec::new();
            for value in values {
                if !unique.contains(&value) {
                    unique.push(value);
                }
            }
            keybinds.insert("input.undo".into(), Json::String(unique.join(",")));
        }
    }
    let lookup = BindingLookup::new(keybind::parse(&keybinds)?)?;
    let attention = group(info, "attention");
    let session = group(info, "session");
    let cursor = group(info, "cursor");
    let theme = group(info, "theme").map(|t| ThemeConfig { name: str_at(Some(t), "name"), mode: str_at(Some(t), "mode") });
    let diffs = group(info, "diffs").map(|d| Diffs {
        source: str_at(Some(d), "source"),
        wrap: str_at(Some(d), "wrap"),
        tree: bool_at(Some(d), "tree"),
        single: bool_at(Some(d), "single"),
        fullscreen: bool_at(Some(d), "fullscreen"),
        view: str_at(Some(d), "view"),
    });
    let terminal = group(info, "terminal").map(|t| Terminal { title: bool_at(Some(t), "title"), copy: str_at(Some(t), "copy") });
    let prompt = group(info, "prompt").map(|p| Prompt {
        editor: bool_at(Some(p), "editor"),
        paste: str_at(Some(p), "paste"),
        image_preview: bool_at(Some(p), "image_preview"),
    });
    let debug = group(info, "debug").map(|d| Debug {
        devtools: bool_at(Some(d), "devtools"),
        timing: bool_at(Some(d), "timing"),
        turn_tokens: d.get("turn_tokens").cloned(),
    });
    let scroll = group(info, "scroll").map(|s| Scroll { speed: num_at(Some(s), "speed"), acceleration: bool_at(Some(s), "acceleration") });
    let enabled = tabs_mode == "on" || (tabs_mode == "auto" && options.herdr_env.as_deref() != Some("1"));
    Ok(Resolved {
        info: input.clone(),
        theme,
        keybinds: lookup,
        plugins: match info.get("plugins") {
            Some(Json::Array(items)) => Some(items.clone()),
            _ => None,
        },
        leader: Leader { timeout: num_at(group(info, "leader"), "timeout").unwrap_or(2000.0) },
        scroll,
        attention: Attention {
            notifications: bool_at(attention, "notifications").unwrap_or(false),
            sound: bool_at(attention, "sound").unwrap_or(false),
            volume: num_at(attention, "volume").unwrap_or(0.4),
            sound_pack: str_at(attention, "sound_pack").unwrap_or_else(|| "opencode.default".into()),
            sounds: group(attention.unwrap_or(&empty), "sounds")
                .map(|s| {
                    s.iter()
                        .filter_map(|(k, v)| match v {
                            Json::String(path) => Some((k.clone(), path.clone())),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        diffs,
        terminal,
        prompt,
        session: Session {
            sidebar: str_at(session, "sidebar"),
            scrollbar: bool_at(session, "scrollbar"),
            submit_scroll: str_at(session, "submit_scroll"),
            thinking: str_at(session, "thinking"),
            grouping: str_at(session, "grouping"),
            verbosity: str_at(session, "verbosity"),
            image_preview: bool_at(session, "image_preview"),
            tps: bool_at(session, "tps").unwrap_or(true),
            markdown: str_at(session, "markdown"),
            new_location: str_at(session, "new_location").unwrap_or_else(|| "launch".into()),
            permissions: str_at(session, "permissions").unwrap_or_else(|| "prompt".into()),
        },
        tabs: Tabs {
            mode: tabs_mode,
            enabled,
            scope: str_at(tabs, "scope").unwrap_or_else(|| "cwd".into()),
            layout: str_at(tabs, "layout").unwrap_or_else(|| "horizontal".into()),
            indicators: str_at(tabs, "indicators").unwrap_or_else(|| "status".into()),
        },
        mini: info.get("mini").cloned(),
        debug,
        experimental: group(info, "experimental").map(|e| {
            crate::theme::js::entries(e)
                .into_iter()
                .filter_map(|(k, v)| match v {
                    Json::Bool(b) => Some((k.to_owned(), *b)),
                    _ => None,
                })
                .collect()
        }),
        animations: bool_at(Some(info), "animations"),
        highlights: bool_at(Some(info), "highlights"),
        mouse: bool_at(Some(info), "mouse").unwrap_or(true),
        cursor: Cursor {
            style: str_at(cursor, "style").unwrap_or_else(|| "block".into()),
            blinking: bool_at(cursor, "blinking").unwrap_or(true),
        },
    })
}

/// The resolved config as JSON in `Config.Resolved`'s shape (the keybinds as their bindings), for the harness.
pub fn to_json(resolved: &Resolved) -> Json {
    let mut out = match &resolved.info {
        Json::Object(o) => o.clone(),
        _ => Object::new(),
    };
    let s = |v: &str| Json::String(v.to_owned());
    let obj = |entries: Vec<(&str, Json)>| Json::Object(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect());
    let a = &resolved.attention;
    out.insert(
        "attention".into(),
        obj(vec![
            ("notifications", Json::Bool(a.notifications)),
            ("sound", Json::Bool(a.sound)),
            ("volume", Json::Number(a.volume)),
            ("sound_pack", s(&a.sound_pack)),
            ("sounds", Json::Object(a.sounds.iter().map(|(k, v)| (k.clone(), s(v))).collect())),
        ]),
    );
    out.insert(
        "keybinds".into(),
        Json::Array(resolved.keybinds.bindings().iter().map(|b| Json::Object(b.0.clone())).collect()),
    );
    out.insert("leader".into(), obj(vec![("timeout", Json::Number(resolved.leader.timeout))]));
    out.insert("mouse".into(), Json::Bool(resolved.mouse));
    out.insert("cursor".into(), obj(vec![("style", s(&resolved.cursor.style)), ("blinking", Json::Bool(resolved.cursor.blinking))]));
    let mut session = match resolved.info.as_object().and_then(|o| o.get("session")) {
        Some(Json::Object(o)) => o.clone(),
        _ => Object::new(),
    };
    session.insert("new_location".into(), s(&resolved.session.new_location));
    session.insert("permissions".into(), s(&resolved.session.permissions));
    session.insert("tps".into(), Json::Bool(resolved.session.tps));
    out.insert("session".into(), Json::Object(session));
    let mut tabs = match resolved.info.as_object().and_then(|o| o.get("tabs")) {
        Some(Json::Object(o)) => o.clone(),
        _ => Object::new(),
    };
    let t = &resolved.tabs;
    tabs.insert("mode".into(), s(&t.mode));
    tabs.insert("enabled".into(), Json::Bool(t.enabled));
    tabs.insert("scope".into(), s(&t.scope));
    tabs.insert("layout".into(), s(&t.layout));
    tabs.insert("indicators".into(), s(&t.indicators));
    out.insert("tabs".into(), Json::Object(tabs));
    Json::Object(out)
}
