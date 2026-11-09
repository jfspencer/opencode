//! App's command registry (`app.tsx`: `appCommands` and the binding layers `App` creates).
//!
//! TODO(port-tui): the dialogs slice fills in the commands as their dialogs land; the session, tabs and prompt
//! slices own the behaviour of theirs.
use std::rc::Rc;

use crate::ctx::Tui;
use crate::keymap::{Bind, Command, Enabled, Layer, create_layer};

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

/// App's commands and binding layers, created in `<App>`'s body in TS's order.
pub fn install(t: &Tui) {
    let commands = {
        let t = t.clone();
        move || {
            let exit = t.exit.clone();
            vec![app("app.exit", "Exit the app", "System", move || exit()).slash("exit", &["quit", "q"])]
        }
    };
    create_layer(move || Layer { mode: Some("global"), commands: commands(), ..Layer::default() });
    create_layer(|| Layer { bindings: APP_BINDING_COMMANDS.to_vec(), ..Layer::default() });
    create_layer(|| Layer { mode: Some("global"), bindings: APP_GLOBAL_BINDING_COMMANDS.to_vec(), ..Layer::default() });
    // TODO(port-tui): the tabs slice: `sessionTabs.enabled`.
    create_layer(|| Layer {
        mode: Some("global"),
        enabled: Some(Enabled::Bool(false)),
        bindings: SESSION_TAB_BINDING_COMMANDS.to_vec(),
        ..Layer::default()
    });
    create_layer(|| Layer {
        mode: Some("global"),
        enabled: Some(Enabled::Fn(Rc::new(|| true))),
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
