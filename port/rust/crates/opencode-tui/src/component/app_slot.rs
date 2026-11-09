//! The built-in feature plugins' contributions to App's `<Slot path="app" />`, in `plugin/builtins.ts` order
//! (each one's layer newer than the last): `prompt/btw.tsx` (`session.aside`), `system/plugins.tsx`
//! (`plugins.list`), `system/stats.tsx` (`stats.open`) and `system/diff-viewer.tsx` (`diff.open`).
//!
//! TODO(port-tui): the commands run stand-ins until their features land: the tools slice (`diff.open`, the diff
//! viewer), and this slice (the plugins dialog, the stats page).
use opencode_tui_core::solid::View;

use crate::keymap::{Command, Layer, create_layer};

fn plugin_layer(command: Command) {
    create_layer(move || Layer { mode: Some("global"), commands: vec![command.clone()], ..Layer::default() });
}

/// `<Slot path="app" />`.
pub fn app_slot() -> View {
    crate::prompt::footer::install_btw();
    plugin_layer(Command::new("plugins.list", || {}).title("Plugins").group("System").slash("plugins", &[]).palette(true));
    plugin_layer(Command::new("stats.open", || {}).title("Usage statistics").group("System").slash("stats", &[]).palette(true));
    plugin_layer(Command::new("diff.open", || {}).title("Open diff viewer").slash("diff", &[]).group("VCS").palette(true));
    View::Empty
}
