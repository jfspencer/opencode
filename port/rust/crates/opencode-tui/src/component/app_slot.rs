//! The built-in feature plugins' contributions to App's `<Slot path="app" />`, in `plugin/builtins.ts` order
//! (each one's layer newer than the last): `prompt/btw.tsx` (`session.aside`), `system/plugins.tsx`
//! (`plugins.list`), `system/stats.tsx` (`stats.open`) and `system/diff-viewer.tsx` (`diff.open`).
//!
//! TODO(port-tui): the plugins dialog and the stats page run stand-ins until those features land; `diff.open` opens
//! the diff viewer beside the session.
use opencode_tui_core::solid::View;

use crate::ctx::{PanelTarget, Route, tui};
use crate::keymap::{Command, Layer, create_layer};

fn plugin_layer(command: Command) {
    create_layer(move || Layer { mode: Some("global"), commands: vec![command.clone()], ..Layer::default() });
}

/// `<Slot path="app" />`.
pub fn app_slot() -> View {
    crate::prompt::footer::install_btw();
    plugin_layer(Command::new("plugins.list", || {}).title("Plugins").group("System").slash("plugins", &[]).palette(true));
    plugin_layer(Command::new("stats.open", || {}).title("Usage statistics").group("System").slash("stats", &[]).palette(true));
    plugin_layer({
        let t = tui();
        let route = t.route;
        let panel = t.panel;
        let fullscreen = t.panel_fullscreen;
        let config = t.config;
        let dialog = crate::ui::dialog::use_dialog();
        Command::new("diff.open", move || {
            dialog.clear();
            let Route::Session { session_id } = route.get_untracked() else { return };
            if panel.get_untracked().as_ref().is_some_and(|current| current.name == "diff" && current.session_id == session_id) {
                panel.set(None);
                fullscreen.set(false);
                return;
            }
            fullscreen.set(config.diffs.as_ref().and_then(|d| d.fullscreen).unwrap_or(false));
            panel.set(Some(PanelTarget { name: "diff".into(), session_id }));
        })
        .title("Open diff viewer")
        .slash("diff", &[])
        .group("VCS")
        .palette(true)
    });
    View::Empty
}
