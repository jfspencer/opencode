//! `component/dialog-config.tsx`: the settings (`settings`, `settingID`) and `DialogConfig`, which shows each
//! setting's value and cycles it (enter, ←/→), writing cli.json through `config.update`.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::reactive::*;
use opencode_tui_core::solid::*;

use crate::ctx::tui;
use crate::keymap::Command;
use crate::ui::dialog::{DialogContext, use_dialog, use_new_designs};
use crate::ui::dialog_select::{Detail, Footer, FooterHint, SelectOption, SelectProps, dialog_select, dialog_select_choice};

/// A value of a setting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum V {
    S(&'static str),
    B(bool),
    N(f64),
}

impl V {
    fn json(self) -> Json {
        match self {
            V::S(s) => Json::String(s.to_string()),
            V::B(b) => Json::Bool(b),
            V::N(n) => Json::Number(n),
        }
    }
}

/// `Setting`.
pub struct Setting {
    pub title: &'static str,
    pub category: &'static str,
    pub path: &'static [&'static str],
    /// Shown below the list while the setting is selected, under the "new-designs" experiment.
    pub description: &'static str,
    pub default: V,
    pub values: &'static [V],
    pub labels: &'static [&'static str],
    pub step: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub format: Option<fn(&Json) -> String>,
    pub keywords: &'static [&'static str],
    /// Under the "new-designs" experiment, enter opens this picker instead of cycling.
    pub open: Option<fn(&DialogContext)>,
}

const OFF_ON: &[&str] = &["off", "on"];
const BOOL: &[V] = &[V::B(false), V::B(true)];

const fn toggle(
    title: &'static str,
    category: &'static str,
    path: &'static [&'static str],
    description: &'static str,
    default: bool,
    keywords: &'static [&'static str],
) -> Setting {
    Setting {
        title,
        category,
        path,
        description,
        default: V::B(default),
        values: BOOL,
        labels: OFF_ON,
        step: None,
        min: None,
        max: None,
        format: None,
        keywords,
        open: None,
    }
}

const fn choice(
    title: &'static str,
    category: &'static str,
    path: &'static [&'static str],
    description: &'static str,
    default: &'static str,
    values: &'static [V],
    labels: &'static [&'static str],
    keywords: &'static [&'static str],
) -> Setting {
    Setting {
        title,
        category,
        path,
        description,
        default: V::S(default),
        values,
        labels,
        step: None,
        min: None,
        max: None,
        format: None,
        keywords,
        open: None,
    }
}

fn number(value: &Json) -> f64 {
    match value {
        Json::Number(n) => *n,
        Json::String(s) => s.parse().unwrap_or(f64::NAN),
        Json::Bool(b) => *b as u8 as f64,
        _ => f64::NAN,
    }
}

fn open_theme_list(dialog: &DialogContext) {
    let back = dialog.clone();
    dialog.show(move || {
        let back = back.clone();
        crate::component::dialog_theme_list::dialog_theme_list(Some(Rc::new(move || {
            back.show(|| component(|| dialog_config(Some("theme.name".to_string()))));
        })))
    });
}

/// `settings`.
pub static SETTINGS: &[Setting] = &[
    Setting {
        title: "Theme",
        category: "Appearance",
        path: &["theme", "name"],
        description: "Color theme used across the interface",
        default: V::S("opencode"),
        values: &[],
        labels: &[],
        step: None,
        min: None,
        max: None,
        format: None,
        keywords: &["color scheme", "colors"],
        open: Some(open_theme_list),
    },
    choice(
        "Color mode",
        "Appearance",
        &["theme", "mode"],
        "Dark or light variant, or follow the system",
        "system",
        &[V::S("system"), V::S("dark"), V::S("light")],
        &[],
        &["dark mode", "light mode", "system theme"],
    ),
    toggle("Animations", "Appearance", &["animations"], "Whether spinners and transitions animate", false, &["motion", "effects"]),
    choice("Sidebar", "Session", &["session", "sidebar"], "Whether the sidebar shows when there is room", "auto", &[V::S("hide"), V::S("auto")], &[], &["side panel"]),
    toggle("Scrollbar", "Session", &["session", "scrollbar"], "Whether a scrollbar is shown next to the chat", false, &["scroll bar"]),
    choice(
        "Scroll on send",
        "Session",
        &["session", "submit_scroll"],
        "Whether sending a prompt jumps to the latest message",
        "bottom",
        &[V::S("bottom"), V::S("keep")],
        &["jump to latest", "keep position"],
        &["submit", "scroll position", "jump to latest", "follow"],
    ),
    choice(
        "Thinking",
        "Session",
        &["session", "thinking"],
        "Whether model thinking is shown in the chat",
        "hide",
        &[V::S("hide"), V::S("show")],
        &[],
        &["reasoning", "chain of thought"],
    ),
    choice(
        "Markdown",
        "Session",
        &["session", "markdown"],
        "Whether chat markdown is rendered or shown as source",
        "rendered",
        &[V::S("source"), V::S("rendered")],
        &[],
        &["syntax", "concealment", "rendering"],
    ),
    choice(
        "Tool grouping",
        "Session",
        &["session", "grouping"],
        "Whether consecutive reads and searches are grouped",
        "auto",
        &[V::S("none"), V::S("auto")],
        &[],
        &["transcript", "messages", "reads", "searches"],
    ),
    choice(
        "Verbosity",
        "Session",
        &["session", "verbosity"],
        "How much tool activity is shown in the chat",
        "medium",
        &[V::S("low"), V::S("medium"), V::S("high")],
        &[],
        &["detail", "activity", "summary", "transcript"],
    ),
    toggle("Transcript images", "Session", &["session", "image_preview"], "Whether images are shown in the chat", false, &["attachments", "images", "tool output"]),
    toggle("TPS", "Session", &["session", "tps"], "Whether tokens per second are shown for responses", true, &["tokens per second", "throughput"]),
    choice(
        "New session location",
        "Session",
        &["session", "new_location"],
        "Directory new sessions start in",
        "launch",
        &[V::S("launch"), V::S("inherit")],
        &["launch directory", "active session"],
        &["directory", "cwd", "inherit"],
    ),
    choice(
        "Permissions",
        "Session",
        &["session", "permissions"],
        "Whether permission requests ask you or auto accept",
        "prompt",
        &[V::S("prompt"), V::S("autoaccept")],
        &["prompt", "auto accept"],
        &["approve", "accept", "permission requests"],
    ),
    choice("Mode", "Tabs", &["tabs", "mode"], "Whether sessions open in tabs", "auto", &[V::S("off"), V::S("on"), V::S("auto")], &[], &[]),
    choice(
        "Scope",
        "Tabs",
        &["tabs", "scope"],
        "Whether tabs are per directory or shared everywhere",
        "cwd",
        &[V::S("cwd"), V::S("global")],
        &["current directory", "global"],
        &[],
    ),
    choice(
        "Layout",
        "Tabs",
        &["tabs", "layout"],
        "Whether tabs run along the top or down the side",
        "horizontal",
        &[V::S("horizontal"), V::S("vertical")],
        &[],
        &["sidebar", "orientation", "left"],
    ),
    choice(
        "Indicators",
        "Tabs",
        &["tabs", "indicators"],
        "Whether tabs show status icons or always numbers",
        "status",
        &[V::S("status"), V::S("numbers")],
        &["status icons", "always show numbers"],
        &["tab numbers", "number mode", "status icons"],
    ),
    choice(
        "Layout",
        "Diffs",
        &["diffs", "view"],
        "Whether diffs are side by side or in one column",
        "auto",
        &[V::S("auto"), V::S("split"), V::S("unified")],
        &[],
        &["diff layout", "split diff", "unified diff"],
    ),
    choice(
        "Wrapping",
        "Diffs",
        &["diffs", "wrap"],
        "Whether long lines in diffs wrap",
        "word",
        &[V::S("none"), V::S("word")],
        &[],
        &["diff wrap", "word wrap", "line wrap"],
    ),
    toggle("File tree", "Diffs", &["diffs", "tree"], "Whether a file tree is shown next to diffs", true, &["diff files"]),
    toggle("Single patch", "Diffs", &["diffs", "single"], "Whether diffs show one file at a time", false, &["one file", "selected file"]),
    Setting {
        title: "Scroll speed",
        category: "Input",
        path: &["scroll", "speed"],
        description: "How far each scroll step moves",
        default: V::N(3.0),
        values: &[],
        labels: &[],
        step: Some(0.25),
        min: Some(0.25),
        max: Some(10.0),
        format: Some(|value| format!("{:.2}", number(value))),
        keywords: &["scrolling"],
        open: None,
    },
    toggle("Acceleration", "Input", &["scroll", "acceleration"], "Whether scrolling speeds up the faster you scroll", false, &["scroll acceleration"]),
    toggle("Mouse", "Input", &["mouse"], "Whether the interface responds to the mouse", true, &["mouse capture"]),
    toggle(
        "Editor context",
        "Input",
        &["prompt", "editor"],
        "Whether your editor's selection is added to prompts",
        true,
        &["file context", "prompt context", "editor selection"],
    ),
    choice(
        "Large pastes",
        "Input",
        &["prompt", "paste"],
        "Whether large pastes are summarized or shown in full",
        "compact",
        &[V::S("compact"), V::S("full")],
        &[],
        &["paste summary", "clipboard", "pasted content"],
    ),
    toggle(
        "Image previews",
        "Input",
        &["prompt", "image_preview"],
        "Whether attached images are previewed in the prompt",
        false,
        &["attachments", "clipboard", "images", "prompt"],
    ),
    Setting {
        title: "Leader timeout",
        category: "Input",
        path: &["leader", "timeout"],
        description: "How long the leader key waits for the next key",
        default: V::N(2000.0),
        values: &[],
        labels: &[],
        step: Some(250.0),
        min: Some(250.0),
        max: Some(10000.0),
        format: Some(|value| format!("{} ms", js_number(number(value)))),
        keywords: &["leader key", "shortcut timeout"],
        open: None,
    },
    toggle(
        "Notifications",
        "Alerts",
        &["attention", "notifications"],
        "Whether to notify when a session needs attention",
        false,
        &["system notifications", "desktop notifications", "alerts"],
    ),
    toggle("Sounds", "Alerts", &["attention", "sound"], "Whether a sound plays when a session needs attention", false, &["audio", "sound effects", "alerts"]),
    Setting {
        title: "Volume",
        category: "Alerts",
        path: &["attention", "volume"],
        description: "Volume of alert sounds",
        default: V::N(0.4),
        values: &[],
        labels: &[],
        step: Some(0.1),
        min: Some(0.0),
        max: Some(1.0),
        format: Some(|value| format!("{}%", (number(value) * 100.0).round())),
        keywords: &["sound volume", "audio volume"],
        open: None,
    },
    toggle(
        "Release highlights",
        "Alerts",
        &["highlights"],
        "Whether release highlights are shown after an update",
        true,
        &["what's new", "changelog", "release notes"],
    ),
    toggle("Window title", "Terminal", &["terminal", "title"], "Whether the window title shows the current session", true, &["terminal title", "tab title"]),
    choice(
        "Copy behavior",
        "Terminal",
        &["terminal", "copy"],
        "Whether selecting text copies it",
        "select",
        &[V::S("manual"), V::S("select")],
        &[],
        &["selection", "clipboard"],
    ),
    toggle("Developer tools", "Debug", &["debug", "devtools"], "Whether the developer tools bar is shown", false, &["debug bar", "developer tools"]),
];

/// `settingID(setting)`.
pub fn setting_id(setting: &Setting) -> String {
    setting.path.join(".")
}

/// A JS number as `String(n)` prints it (integers without a fraction).
pub fn js_number(n: f64) -> String {
    if n.is_finite() && n == n.trunc() && n.abs() < 1e21 {
        return format!("{}", n as i64);
    }
    let text = format!("{n}");
    text
}

fn js_string(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => js_number(*n),
        Json::Null => "null".into(),
        _ => opencode_tui_sync::client::stringify(value),
    }
}

/// The value at a setting's path in the resolved config, else its default.
fn value(setting: &Setting) -> Json {
    let data = crate::config::resolve::to_json(&tui().config_context.data());
    let mut current = Some(&data);
    for key in setting.path {
        current = current.and_then(|v| v.as_object()).and_then(|o| o.get(*key));
    }
    if setting_id(setting) == "theme.name" {
        // TODO(port-tui): `themes.selected` of the theme context.
        return current.cloned().unwrap_or_else(|| Json::String(tui().config.theme.as_ref().and_then(|t| t.name.clone()).unwrap_or_else(|| "opencode".into())));
    }
    current.cloned().filter(|v| !matches!(v, Json::Null)).unwrap_or(setting.default.json())
}

fn values(setting: &Setting) -> Vec<Json> {
    if setting_id(setting) == "theme.name" {
        return crate::component::dialog_theme_list::theme_names();
    }
    setting.values.iter().map(|v| v.json()).collect()
}

fn display(setting: &Setting) -> String {
    let current = value(setting);
    if let Some(format) = setting.format {
        return format(&current);
    }
    let index = setting.values.iter().position(|v| v.json() == current);
    match index {
        Some(i) => setting.labels.get(i).map(|l| l.to_string()).unwrap_or_else(|| js_string(&current)),
        None => js_string(&current),
    }
}

/// `<DialogConfig current>`.
pub fn dialog_config(current: Option<String>) -> View {
    let config = tui().config_context;
    let toast = tui().toast;
    let new_designs = use_new_designs();
    let dialog = use_dialog();
    let current = SETTINGS.iter().position(|s| Some(setting_id(s)) == current).unwrap_or(0);
    let (selected, set_selected) = create_signal(current);
    let (saving, set_saving) = create_signal(false);
    let options = {
        let new_designs = new_designs.clone();
        create_memo(move || {
            SETTINGS
                .iter()
                .enumerate()
                .map(|(index, setting)| {
                    let mut option = SelectOption::new(setting.title, index);
                    option.category = Some(setting.category.to_string());
                    option.search_text = (!setting.keywords.is_empty()).then(|| setting.keywords.join(" "));
                    let shown = display(setting);
                    option.footer = Some(if new_designs() {
                        let open = setting.open.is_some();
                        Footer::View(Rc::new(move || {
                            let shown = shown.clone();
                            dialog_select_choice(move || shown.clone(), move || selected.get() == index, open)
                        }))
                    } else {
                        Footer::Text(shown)
                    });
                    option.footer_overhang = new_designs().then_some(2.0);
                    option.detail = new_designs().then(|| Detail::Text(setting.description.to_string()));
                    option
                })
                .collect::<Vec<_>>()
        })
    };
    let change: Rc<dyn Fn(i64, Option<usize>)> = Rc::new(move |direction: i64, index: Option<usize>| {
        if saving.get_untracked() {
            return;
        }
        let setting = &SETTINGS[index.unwrap_or_else(|| selected.get_untracked())];
        let current = value(setting);
        let choices = values(setting);
        let next = if !choices.is_empty() {
            let at = choices.iter().position(|c| *c == current).map(|i| i as i64).unwrap_or(-1);
            let count = choices.len() as i64;
            choices[((at + direction + count) % count) as usize].clone()
        } else {
            let step = setting.step.unwrap_or(1.0);
            Json::Number(setting.max.unwrap_or(f64::MAX).min(setting.min.unwrap_or(f64::MIN).max(number(&current) + direction as f64 * step)))
        };
        if next == current {
            return;
        }
        set_saving.set(true);
        let result = config.update(|draft| {
            let mut target = draft;
            for key in &setting.path[..setting.path.len() - 1] {
                if !matches!(target, Json::Object(_)) {
                    *target = Json::Object(Default::default());
                }
                let Json::Object(object) = target else { unreachable!() };
                let entry = object.entry(key.to_string()).or_insert_with(|| Json::Object(Default::default()));
                if !matches!(entry, Json::Object(_)) {
                    *entry = Json::Object(Default::default());
                }
                target = entry;
            }
            if let Json::Object(object) = target {
                object.insert(setting.path[setting.path.len() - 1].to_string(), next.clone());
            }
        });
        if let Err(error) = result {
            toast.error(error);
        }
        set_saving.set(false);
    });
    let mut props = SelectProps::new("Settings", move || options.get());
    props.search_in_title = true;
    props.title_placeholder = Some("Search settings".into());
    // Under new designs the opening setting is focused without the current-item marker; changing a value keeps the
    // selection where it is.
    let designs = new_designs.clone();
    props.current = Some(Rc::new(move || (!designs()).then_some(current)));
    let designs = new_designs.clone();
    props.focus_target = Some(Rc::new(move || designs().then_some(current)));
    props.preserve_selection = new_designs();
    props.filter_threshold = Some(0.7);
    props.on_move = Some(Rc::new(move |option: &SelectOption<usize>| set_selected.set(option.value)));
    {
        let change = change.clone();
        let new_designs = new_designs.clone();
        let dialog = dialog.clone();
        props.on_select = Some(Rc::new(move |option: &SelectOption<usize>| {
            if let Some(open) = SETTINGS[option.value].open.filter(|_| new_designs()) {
                open(&dialog);
                return;
            }
            change(1, Some(option.value));
        }));
    }
    if !new_designs() {
        props.footer_hints = vec![FooterHint { title: "change".into(), label: "←/→".into(), side: None, group: None }];
    }
    let previous = change.clone();
    let next = change.clone();
    props.bindings = vec![
        Command::inline("left", move || {
            previous(-1, None);
            true
        })
        .title("Previous value")
        .group("Settings"),
        Command::inline("right", move || {
            next(1, None);
            true
        })
        .title("Next value")
        .group("Settings"),
    ];
    dialog_select(props)
}
