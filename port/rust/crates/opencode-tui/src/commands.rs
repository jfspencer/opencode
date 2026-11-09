//! App's command registry (`app.tsx`: `appCommands` and the binding layers `App` creates).
//!
//! TODO(port-tui): commands whose behaviour another slice owns run a stand-in until it lands: the session slice
//! (`session.new`, `session.clear`), the tabs slice (the tab and quick-switch commands), the tools slice
//! (`diff.open`), and the dialogs not ported yet (each marked where it is).
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;

use crate::ctx::{Route, Tui};
use crate::keymap::{Bind, COMMAND_PALETTE_COMMAND, Command, Enabled, Layer, create_layer};
use crate::ui::dialog::{DialogContext, use_dialog};

const APP_GLOBAL_BINDING_COMMANDS: [&str; 4] = ["session.list", "session.new", "open.menu", "terminal.suspend"];

const SESSION_TAB_BINDING_COMMANDS: [&str; 18] = [
    "session.tab.next",
    "session.tab.previous",
    "session.tab.history.back",
    "session.tab.history.forward",
    "session.tab.next_unread",
    "session.tab.previous_unread",
    "session.tab.close",
    "session.tab.reopen",
    "session.tab.select.1",
    "session.tab.select.2",
    "session.tab.select.3",
    "session.tab.select.4",
    "session.tab.select.5",
    "session.tab.select.6",
    "session.tab.select.7",
    "session.tab.select.8",
    "session.tab.select.9",
    "session.tab.select.10",
];

const PINNED_SESSION_BINDING_COMMANDS: [&str; 9] = [
    "session.quick_switch.1",
    "session.quick_switch.2",
    "session.quick_switch.3",
    "session.quick_switch.4",
    "session.quick_switch.5",
    "session.quick_switch.6",
    "session.quick_switch.7",
    "session.quick_switch.8",
    "session.quick_switch.9",
];

const APP_BINDING_COMMANDS: [&str; 34] = [
    "command.palette.show",
    "model.list",
    "model.cycle_recent",
    "model.cycle_recent_reverse",
    "model.cycle_favorite",
    "model.cycle_favorite_reverse",
    "agent.list",
    "mcp.list",
    "usage.show",
    "agent.cycle",
    "agent.cycle.reverse",
    "variant.cycle",
    "variant.list",
    "provider.connect",
    "opencode.settings",
    "opencode.status",
    "opencode.update",
    "server.pair",
    "service.restart",
    "location.reload",
    "opencode.debug",
    "theme.switch",
    "theme.switch_mode",
    "theme.mode.lock",
    "help.show",
    "docs.open",
    "diff.open",
    "app.debug",
    "app.console",
    "terminal.title.toggle",
    "app.toggle.animations",
    "app.toggle.file_context",
    "app.toggle.diffwrap",
    "app.toggle.paste_summary",
];

/// `.map(({ name, category, ...command }) => ({ id: name, group: category, bind: false, palette: true, ...command }))`.
fn app(id: &str, title: &str, group: &str, run: impl Fn() + 'static) -> Command {
    Command::new(id, run).title(title).group(group).bind(Bind::Off).palette(true)
}

/// `dialog.replace(() => <Component />)`.
fn open(dialog: &DialogContext, view: fn() -> View) -> impl Fn() + 'static {
    let dialog = dialog.clone();
    move || dialog.show(move || component(view))
}

/// A command whose behaviour is not ported yet.
fn stand_in() {}

/// `hasConnectedProvider(data.location.integration.list() ?? [])`.
fn connected(t: &Tui) -> bool {
    let Some(directory) = t.data.directory.get() else { return false };
    let location = Json::Object([("directory".to_string(), Json::String(directory))].into_iter().collect());
    let list = t.data.store.location.integration.list(Some(&location));
    list.as_ref().map(|l| crate::data::array(l)).unwrap_or(&[]).iter().any(|integration| {
        crate::data::get(integration, "connections").map(crate::data::array).is_some_and(|c| !c.is_empty())
    })
}

/// A toggle of cli.json from a command (`config.update(...).catch(toast.error)`).
fn update_config(t: &Tui, mutate: impl FnOnce(&mut Json)) {
    if let Err(error) = t.config_context.update(mutate) {
        t.toast.error(error);
    }
}

fn set_path(draft: &mut Json, path: &[&str], value: Json) {
    let mut target = draft;
    for key in &path[..path.len() - 1] {
        if !matches!(target, Json::Object(_)) {
            *target = Json::Object(Default::default());
        }
        let Json::Object(object) = target else { return };
        target = object.entry(key.to_string()).or_insert_with(|| Json::Object(Default::default()));
    }
    if !matches!(target, Json::Object(_)) {
        *target = Json::Object(Default::default());
    }
    if let Json::Object(object) = target {
        object.insert(path[path.len() - 1].to_string(), value);
    }
}

/// `appCommands()`.
fn app_commands(t: &Tui, dialog: &DialogContext, local: &crate::local::Local) -> Vec<Command> {
    let data = t.data.clone();
    let route = t.route;
    let config = t.config_context.data();
    let themes = crate::themes::use_themes();
    let tabs_enabled = config.tabs.enabled;
    let mut commands = vec![
        app(COMMAND_PALETTE_COMMAND, "Show command palette", "System", open(dialog, crate::component::command_palette::command_palette))
            .palette(false),
        // TODO(port-tui): DialogSessionList.
        app("session.list", "Switch session", "Session", stand_in)
            .suggested(!data.store.session.list().is_empty())
            .slash("sessions", &["resume", "continue"]),
        // TODO(port-tui): the session slice (route to home in the new session's location).
        app("session.new", "New session", "Session", {
            let dialog = dialog.clone();
            move || {
                route.set(Route::Home);
                dialog.clear();
            }
        })
        .suggested(route.with(|r| matches!(r, Route::Session { .. })))
        .slash("new", &[]),
        app("session.clear", "Clear session", "Session", {
            let dialog = dialog.clone();
            move || {
                route.set(Route::Home);
                dialog.clear();
            }
        })
        .slash("clear", &[]),
        // TODO(port-tui): DialogOpen.
        app("open.menu", "Open session or project", "Session", stand_in).slash("open", &["projects", "project"]),
    ];
    for i in 1..=9 {
        let quick = local.clone();
        commands.push(
            app(&format!("session.quick_switch.{i}"), &format!("Switch to session in quick slot {i}"), "Session", move || {
                quick.quick_switch(i)
            })
            .palette(false)
            .enabled_fn(move || !tabs_enabled),
        );
    }
    for (id, title) in [
        ("session.tab.next", "Next tab"),
        ("session.tab.previous", "Previous tab"),
        ("session.tab.history.back", "Back in tab history"),
        ("session.tab.history.forward", "Forward in tab history"),
        ("session.tab.next_unread", "Next unread tab"),
        ("session.tab.previous_unread", "Previous unread tab"),
    ] {
        commands.push(app(id, title, "Session", stand_in).palette(false).enabled_fn(move || tabs_enabled));
    }
    commands.push(app("session.tab.close", "Close tab", "Session", stand_in).enabled_fn(move || tabs_enabled));
    commands.push(app("session.tab.reopen", "Reopen closed tab", "Session", stand_in).enabled_fn(move || tabs_enabled));
    for i in 1..=10 {
        commands.push(
            app(&format!("session.tab.select.{i}"), &format!("Switch to tab {i}"), "Session", stand_in)
                .palette(false)
                .enabled_fn(move || tabs_enabled),
        );
    }
    let variants = local.variant_list();
    commands.extend([
        // TODO(port-tui): DialogModel.
        app("model.list", "Switch model", "Agent", stand_in).suggested(true).slash("models", &[]),
        app("model.cycle_recent", "Model cycle", "Agent", {
            let local = local.clone();
            move || local.cycle(1)
        })
        .palette(false),
        app("model.cycle_recent_reverse", "Model cycle reverse", "Agent", {
            let local = local.clone();
            move || local.cycle(-1)
        })
        .palette(false),
        app("model.cycle_favorite", "Favorite cycle", "Agent", {
            let local = local.clone();
            move || local.cycle_favorite(1)
        })
        .palette(false),
        app("model.cycle_favorite_reverse", "Favorite cycle reverse", "Agent", {
            let local = local.clone();
            move || local.cycle_favorite(-1)
        })
        .palette(false),
        // TODO(port-tui): DialogAgent.
        app("agent.list", "Switch agent", "Agent", stand_in).slash("agents", &[]),
        app("usage.show", "View usage", "Agent", open(dialog, crate::usage::dialog_usage)).slash("usage", &[]),
        // TODO(port-tui): DialogMcp.
        app("mcp.list", "MCP servers", "Agent", stand_in).slash("mcps", &[]),
        app("agent.cycle", "Agent cycle", "Agent", {
            let local = local.clone();
            move || local.agent_move(1)
        })
        .palette(false),
        app("variant.cycle", "Variant cycle", "Agent", {
            let local = local.clone();
            move || local.variant_cycle()
        }),
        // TODO(port-tui): DialogVariant.
        app("variant.list", "Switch model variant", "Agent", {
            let toast = t.toast.clone();
            let local = local.clone();
            move || {
                if local.variant_list().is_empty() {
                    toast.show(
                        crate::toast::ToastInput::new(crate::toast::Variant::Info, "The current model does not support any variants.")
                            .title("No variants available"),
                    );
                }
            }
        })
        .palette(!variants.is_empty())
        .slash("variants", &["thinking", "effort"]),
        app("agent.cycle.reverse", "Agent cycle reverse", "Agent", {
            let local = local.clone();
            move || local.agent_move(-1)
        })
        .palette(false),
        // TODO(port-tui): DialogIntegration.
        app("provider.connect", "Connect an integration", "Integration", stand_in)
            .suggested(!connected(t))
            .slash("connect", &[]),
    ]);
    // TODO(port-tui): `opencode.highlights` ("Show release highlights") while the build embeds release highlights
    // (`app.highlights`, stable release builds only); this build has none.
    commands.extend([
        app("opencode.settings", "Open settings", "System", open(dialog, || dialog_config_view()))
            .suggested(true)
            .slash("settings", &[]),
        app("opencode.status", "View status", "System", open(dialog, crate::component::dialog_status::dialog_status))
            .slash("status", &[]),
        // TODO(port-tui): the update notification (`updater.open("manual")`).
        app("opencode.update", "Update OpenCode", "System", stand_in).slash("update", &[]),
        // TODO(port-tui): DialogPair.
        app("server.pair", "Pair device", "System", stand_in).slash("pair", &["web"]),
    ]);
    // TODO(port-tui): `service.restart` while the client manages its service (`client.restart`).
    commands.push(
        app("location.reload", "Reload configuration", "System", {
            let dialog = dialog.clone();
            let toast = t.toast.clone();
            let client = t.data.client.clone();
            let directory = t.data.directory;
            move || {
                dialog.clear();
                toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Info, "Reloading configuration…").duration(30000));
                let toast = toast.clone();
                let client = client.clone();
                let location = directory.get_untracked().map(|d| crate::data::object(vec![("directory", Json::String(d))]));
                tokio::task::spawn_local(async move {
                    let input = crate::data::object(vec![("location", location.unwrap_or(Json::Null))]);
                    match client.call_named("location.reload", &input, Default::default()).await {
                        Ok(_) => toast.show(crate::toast::ToastInput::new(crate::toast::Variant::Success, "Configuration reloaded")),
                        Err(error) => toast.error(error.message()),
                    }
                });
            }
        })
        .slash("reload", &[]),
    );
    let mode_dark = themes.as_ref().is_none_or(|t| t.mode() == crate::theme::Mode::Dark);
    let locked = themes.as_ref().is_some_and(|t| t.locked());
    commands.extend([
        // TODO(port-tui): DialogDebug.
        app("opencode.debug", "View debug info", "System", stand_in).slash("debug", &[]),
        app("theme.switch", "Switch theme", "System", open(dialog, || crate::component::dialog_theme_list::dialog_theme_list(None)))
            .slash("themes", &[]),
        app("theme.switch_mode", if mode_dark { "Switch to light mode" } else { "Switch to dark mode" }, "System", {
            let themes = themes.clone();
            let dialog = dialog.clone();
            move || {
                if let Some(themes) = &themes {
                    let next = if themes.mode() == crate::theme::Mode::Dark { crate::theme::Mode::Light } else { crate::theme::Mode::Dark };
                    themes.set_mode(next, true);
                }
                dialog.clear();
            }
        })
        .palette(false)
        .enabled_fn({
            let themes = themes.clone();
            move || {
                themes.as_ref().is_some_and(|t| {
                    t.supports(if t.mode() == crate::theme::Mode::Dark { crate::theme::Mode::Light } else { crate::theme::Mode::Dark })
                })
            }
        }),
        app("theme.mode.lock", if locked { "Unlock theme mode" } else { "Lock theme mode" }, "System", {
            let themes = themes.clone();
            let dialog = dialog.clone();
            move || {
                if let Some(themes) = &themes {
                    if themes.locked() {
                        themes.unlock();
                    } else {
                        themes.lock();
                    }
                }
                dialog.clear();
            }
        })
        .palette(false),
        app("help.show", "Help", "System", open(dialog, crate::ui::dialog_help::dialog_help)).slash("help", &[]),
        app("docs.open", "Open docs", "System", {
            let dialog = dialog.clone();
            move || {
                // TODO(port-tui): openUrl("https://opencode.ai/docs").
                dialog.clear();
            }
        }),
        app("app.exit", "Exit the app", "System", {
            let exit = t.exit.clone();
            move || exit()
        })
        .slash("exit", &["quit", "q"]),
        // TODO(port-tui): the renderer's debug overlay.
        app("app.debug", "Toggle debug panel", "System", {
            let dialog = dialog.clone();
            move || dialog.clear()
        })
        .palette(false),
        // TODO(port-tui): the renderer's console.
        app("app.console", "Toggle console", "System", {
            let dialog = dialog.clone();
            move || dialog.clear()
        }),
        // TODO(port-tui): renderer.suspend and SIGTSTP.
        app("terminal.suspend", "Suspend terminal", "System", stand_in).palette(false).enabled(true),
    ]);
    let terminal_title = config.terminal.as_ref().and_then(|t| t.title).unwrap_or(true);
    let animations = config.animations.unwrap_or(true);
    let file_context = config.prompt.as_ref().and_then(|p| p.editor).unwrap_or(true);
    let diff_wrap_word = config.diffs.as_ref().and_then(|d| d.wrap.clone()).unwrap_or_else(|| "word".into()) == "word";
    let paste_summary = config.prompt.as_ref().and_then(|p| p.paste.clone()).as_deref() != Some("full");
    let toggle = |id: &str, on: &str, off: &str, enabled: bool, run: Box<dyn Fn()>| {
        let dialog = dialog.clone();
        app(id, if enabled { off } else { on }, "System", move || {
            run();
            dialog.clear();
        })
        .palette(false)
    };
    commands.extend([
        toggle("terminal.title.toggle", "Enable terminal title", "Disable terminal title", terminal_title, {
            let t = t.clone();
            Box::new(move || {
                let next = !terminal_title;
                if !next {
                    renderer::set_terminal_title("");
                }
                update_config(&t, |draft| set_path(draft, &["terminal", "title"], Json::Bool(next)));
            })
        }),
        toggle("app.toggle.animations", "Enable animations", "Disable animations", animations, {
            let t = t.clone();
            Box::new(move || update_config(&t, |draft| set_path(draft, &["animations"], Json::Bool(!animations))))
        }),
        toggle("app.toggle.file_context", "Enable file context", "Disable file context", file_context, {
            let t = t.clone();
            Box::new(move || update_config(&t, |draft| set_path(draft, &["prompt", "editor"], Json::Bool(!file_context))))
        }),
        toggle("app.toggle.diffwrap", "Enable diff wrapping", "Disable diff wrapping", diff_wrap_word, {
            let t = t.clone();
            Box::new(move || {
                let next = if diff_wrap_word { "none" } else { "word" };
                update_config(&t, |draft| set_path(draft, &["diffs", "wrap"], Json::String(next.into())))
            })
        }),
        toggle("app.toggle.paste_summary", "Enable paste summary", "Disable paste summary", paste_summary, {
            let t = t.clone();
            Box::new(move || {
                let next = if paste_summary { "full" } else { "compact" };
                update_config(&t, |draft| set_path(draft, &["prompt", "paste"], Json::String(next.into())))
            })
        }),
    ]);
    commands
}

fn dialog_config_view() -> View {
    crate::component::dialog_config::dialog_config(None)
}

/// App's commands and binding layers, created in `<App>`'s body in TS's order.
pub fn install(t: &Tui) {
    let dialog = use_dialog();
    let local = crate::local::local();
    // --agent and --model name entries of the location's agents and models, which load after mount: a computed
    // applies them before effects read the selection.
    {
        let local = local.clone();
        let args = t.args.clone();
        let toast = t.toast.clone();
        let selected = std::cell::Cell::new(false);
        opencode_tui_core::reactive::create_computed(move || {
            if selected.get() || !local.agent_ready() || !local.model_ready() || !local.catalog_ready() {
                return;
            }
            selected.set(true);
            opencode_tui_core::reactive::batch(|| {
                if let Some(agent) = &args.agent {
                    local.agent_set(agent);
                }
                if let Some(model) = &args.model {
                    let (provider_id, model_id) = crate::data::parse_model(model);
                    if provider_id.is_empty() || model_id.is_empty() {
                        toast.show(
                            crate::toast::ToastInput::new(crate::toast::Variant::Warning, format!("Invalid model format: {model}"))
                                .duration(3000),
                        );
                        return;
                    }
                    local.model_set(&crate::model_preference::ModelRef::new(provider_id, model_id), true);
                }
            });
        });
    }
    {
        let t = t.clone();
        create_layer(move || Layer { mode: Some("global"), commands: app_commands(&t, &dialog, &local), ..Layer::default() });
    }
    create_layer(|| Layer { bindings: APP_BINDING_COMMANDS.to_vec(), ..Layer::default() });
    create_layer(|| Layer { mode: Some("global"), bindings: APP_GLOBAL_BINDING_COMMANDS.to_vec(), ..Layer::default() });
    let config = t.config_context.clone();
    create_layer(move || Layer {
        mode: Some("global"),
        enabled: Some(Enabled::Bool(config.data().tabs.enabled)),
        bindings: SESSION_TAB_BINDING_COMMANDS.to_vec(),
        ..Layer::default()
    });
    let config = t.config_context.clone();
    create_layer(move || Layer {
        mode: Some("global"),
        enabled: Some(Enabled::Bool(!config.data().tabs.enabled)),
        bindings: PINNED_SESSION_BINDING_COMMANDS.to_vec(),
        ..Layer::default()
    });
    let prompt = t.prompt.clone();
    create_layer(move || {
        let prompt = prompt.clone();
        Layer {
            enabled: Some(Enabled::Fn(Rc::new(move || {
                let current = *prompt.borrow();
                match current.filter(|input| input.is_alive() && input.focused()) {
                    None => true,
                    Some(input) => input.textarea_text().is_empty(),
                }
            }))),
            bindings: vec!["app.exit"],
            ..Layer::default()
        }
    });
}
