//! `packages/tui/src/config/keybind.ts` (`TuiKeybind`): the keybind definitions (defaults and descriptions), the
//! overrides' validation (`parse`, `unknownKeys`) and the binding defaults, and `createBindingLookup` of
//! `@opentui/keymap/extras` (`binding-lookup.ts`), which `Config.resolve` builds the bindings with.
//!
//! Binding strings (`"ctrl+x,<leader>q"`) are kept as written: splitting them into key sequences, the leader, and
//! printing them in hints (`formatKeySequence`/`formatCommandBindings`) are `@opentui/keymap`'s (opencode-tui-input).

use std::sync::LazyLock;

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

use super::schema::decode_binding_value;

/// `LeaderDefault`.
pub const LEADER_DEFAULT: &str = "ctrl+x";

/// `Definitions`: name, default binding (JSON) and description, in definition order.
pub const DEFINITIONS: &[(&str, &str, &str)] = &[
    ("leader", r#""ctrl+x""#, "Leader key for keybind combinations"),
    ("app.exit", r#""ctrl+c,ctrl+d,<leader>q""#, "Exit the application"),
    ("app.clear", r#""ctrl+l""#, "Clear the screen in mini"),
    ("app.debug", r#""none""#, "Toggle debug panel"),
    ("app.console", r#""none""#, "Toggle console"),
    ("app.scrap", r#""none""#, "Open scrap screen"),
    ("app.toggle.animations", r#""none""#, "Toggle animations"),
    ("app.toggle.file_context", r#""none""#, "Toggle file context"),
    ("app.toggle.diffwrap", r#""none""#, "Toggle diff wrapping"),
    ("app.toggle.paste_summary", r#""none""#, "Toggle paste summary"),
    ("command.palette.show", r#""ctrl+p""#, "List available commands"),
    ("help.show", r#""none""#, "Open help dialog"),
    ("docs.open", r#""none""#, "Open documentation"),
    ("opencode.settings", r#""none""#, "Open settings"),
    ("server.pair", r#""none""#, "Pair device"),
    ("service.restart", r#""none""#, "Restart service"),
    ("location.reload", r#""none""#, "Reload configuration"),
    ("diff.open", r#""none""#, "Open diff viewer"),
    ("diff.close", r#""escape,q""#, "Close diff viewer"),
    ("diff.down", r#""j,down""#, "Move diff viewer down"),
    ("diff.up", r#""k,up""#, "Move diff viewer up"),
    ("diff.page.down", r#""pagedown,ctrl+f""#, "Page diff viewer down"),
    ("diff.page.up", r#""pageup,ctrl+b""#, "Page diff viewer up"),
    ("diff.half_page.down", r#""ctrl+d""#, "Scroll diff viewer down half a page"),
    ("diff.half_page.up", r#""ctrl+u""#, "Scroll diff viewer up half a page"),
    ("diff.first", r#""gg,home""#, "Go to the start of the diff"),
    ("diff.last", r#""shift+g,end""#, "Go to the end of the diff"),
    ("diff.toggle", r#""none""#, "Deprecated: file tree is mouse-controlled"),
    ("diff.expand", r#""none""#, "Deprecated: file tree is mouse-controlled"),
    ("diff.expand_all", r#""none""#, "Deprecated: file tree is mouse-controlled"),
    ("diff.collapse", r#""none""#, "Deprecated: file tree is mouse-controlled"),
    ("diff.switch_focus", r#""none""#, "Deprecated: keyboard navigation always controls the diff"),
    ("diff.next_hunk", r#""]""#, "Jump to next diff hunk"),
    ("diff.previous_hunk", r#""[""#, "Jump to previous diff hunk"),
    ("diff.next_file", r#""n,alt+down""#, "Jump to next diff file"),
    ("diff.previous_file", r#""p,alt+up""#, "Jump to previous diff file"),
    ("diff.toggle_file_tree", r#""b""#, "Toggle diff viewer file tree"),
    ("diff.single_patch", r#""s""#, "Toggle single patch view"),
    ("diff.switch_source", r#""d""#, "Switch diff viewer source"),
    ("diff.toggle_view", r#""v""#, "Toggle diff viewer split or unified view"),
    ("diff.toggle_fullscreen", r#""f""#, "Toggle diff viewer full screen"),
    ("diff.mark_reviewed", r#""m""#, "Toggle selected diff file reviewed"),
    ("diff.help", r#""?,shift+?,shift+/""#, "Show more diff viewer shortcuts"),
    ("prompt.editor", r#""<leader>e""#, "Open external editor"),
    ("theme.switch", r#""none""#, "List available themes"),
    ("theme.switch_mode", r#""none""#, "Switch between light and dark theme mode"),
    ("theme.mode.lock", r#""none""#, "Lock or unlock theme mode"),
    ("session.sidebar.toggle", r#""<leader>b""#, "Toggle sidebar"),
    ("pane.focus.left", r#""<leader>left""#, "Focus session pane"),
    ("pane.focus.right", r#""<leader>right""#, "Focus right pane"),
    ("terminal.select", r#""<leader>down""#, "Select terminal"),
    ("terminal.toggle", r#""<leader>t""#, "Toggle terminal pane"),
    ("terminal.close", r#""<leader>up""#, "Close terminal pane"),
    ("session.toggle.scrollbar", r#""none""#, "Toggle session scrollbar"),
    ("opencode.status", r#""<leader>s""#, "View status"),
    ("opencode.debug", r#""none""#, "View debug info"),
    ("session.export", r#""<leader>x""#, "Export session to editor"),
    ("session.copy", r#""none""#, "Copy session transcript"),
    ("session.copy.id", r#""none""#, "Copy session ID"),
    ("session.move", r#""none""#, "Manage workspaces"),
    ("session.new", r#""<leader>n""#, "Create a new session"),
    ("session.list", r#""<leader>l""#, "List all sessions"),
    ("open.menu", r#""ctrl+o""#, "Open recent sessions and projects"),
    ("session.tab.next", r#""ctrl+tab,alt+down""#, "Switch to next open session tab"),
    ("session.tab.previous", r#""ctrl+shift+tab,alt+up""#, "Switch to previous open session tab"),
    ("session.tab.history.back", r#""none""#, "Go back in session tab history"),
    ("session.tab.history.forward", r#""ctrl+i""#, "Go forward in session tab history"),
    ("session.tab.next_unread", r#""alt+shift+down""#, "Switch to next unread session tab"),
    ("session.tab.previous_unread", r#""alt+shift+up""#, "Switch to previous unread session tab"),
    ("session.tab.close", r#""<leader>w""#, "Close current session tab"),
    ("session.tab.reopen", r#""ctrl+shift+t""#, "Reopen last closed session tab"),
    ("session.timeline", r#""<leader>g""#, "Show session timeline"),
    ("session.fork", r#""none""#, "Fork session from message"),
    ("session.rename", r#""ctrl+r""#, "Rename session"),
    ("session.delete", r#""ctrl+d""#, "Delete session"),
    ("session.share", r#""none""#, "Deprecated: session sharing was removed"),
    ("session.unshare", r#""none""#, "Deprecated: session sharing was removed"),
    ("session.interrupt", r#""escape""#, "Interrupt current session"),
    ("session.background", r#""ctrl+b""#, "Background blocking session tools"),
    ("session.compact", r#""<leader>c""#, "Compact the session"),
    ("session.aside", r#""none""#, "Ask a side question"),
    ("session.cd", r#""none""#, "Change working directory"),
    ("session.queued_prompts", r#""<leader>q""#, "Manage queued prompts"),
    ("queued_prompt.undo", r#""ctrl+u""#, "Undo queued prompt"),
    ("queued_prompt.delete", r#""ctrl+d""#, "Delete queued prompt"),
    ("session.toggle.exploration_grouping", r#""none""#, "Toggle related tool call grouping"),
    ("session.verbosity.cycle", r#""none""#, "Cycle transcript verbosity"),
    ("session.child.first", r#""down""#, "Toggle subagent picker"),
    ("session.child.next", r#""right""#, "Go to next child session"),
    ("session.child.previous", r#""left""#, "Go to previous child session"),
    ("session.parent", r#""up""#, "Go to parent session"),
    ("session.pin.toggle", r#""ctrl+f""#, "Pin or unpin session in the session list"),
    ("session.quick_switch.1", r#""<leader>1""#, "Switch to session in quick slot 1"),
    ("session.quick_switch.2", r#""<leader>2""#, "Switch to session in quick slot 2"),
    ("session.quick_switch.3", r#""<leader>3""#, "Switch to session in quick slot 3"),
    ("session.quick_switch.4", r#""<leader>4""#, "Switch to session in quick slot 4"),
    ("session.quick_switch.5", r#""<leader>5""#, "Switch to session in quick slot 5"),
    ("session.quick_switch.6", r#""<leader>6""#, "Switch to session in quick slot 6"),
    ("session.quick_switch.7", r#""<leader>7""#, "Switch to session in quick slot 7"),
    ("session.quick_switch.8", r#""<leader>8""#, "Switch to session in quick slot 8"),
    ("session.quick_switch.9", r#""<leader>9""#, "Switch to session in quick slot 9"),
    ("session.tab.select.1", r#""<leader>1,ctrl+1""#, "Switch to session tab 1"),
    ("session.tab.select.2", r#""<leader>2,ctrl+2""#, "Switch to session tab 2"),
    ("session.tab.select.3", r#""<leader>3,ctrl+3""#, "Switch to session tab 3"),
    ("session.tab.select.4", r#""<leader>4,ctrl+4""#, "Switch to session tab 4"),
    ("session.tab.select.5", r#""<leader>5,ctrl+5""#, "Switch to session tab 5"),
    ("session.tab.select.6", r#""<leader>6,ctrl+6""#, "Switch to session tab 6"),
    ("session.tab.select.7", r#""<leader>7,ctrl+7""#, "Switch to session tab 7"),
    ("session.tab.select.8", r#""<leader>8,ctrl+8""#, "Switch to session tab 8"),
    ("session.tab.select.9", r#""<leader>9,ctrl+9""#, "Switch to session tab 9"),
    ("session.tab.select.10", r#""<leader>0,ctrl+0""#, "Switch to session tab 10"),
    ("stash.delete", r#""ctrl+d""#, "Delete stash entry"),
    ("model.dialog.provider", r#""ctrl+a""#, "Open provider list from model dialog"),
    ("model.dialog.favorite", r#""ctrl+f""#, "Toggle model favorite status"),
    ("model.dialog.usage", r#""ctrl+u""#, "Open usage limits from model dialog"),
    ("model.list", r#""<leader>m""#, "List available models"),
    ("model.cycle_recent", r#""f2""#, "Next recently used model"),
    ("model.cycle_recent_reverse", r#""shift+f2""#, "Previous recently used model"),
    ("model.cycle_favorite", r#""none""#, "Next favorite model"),
    ("model.cycle_favorite_reverse", r#""none""#, "Previous favorite model"),
    ("mcp.list", r#""none""#, "List MCP servers"),
    ("usage.show", r#""none""#, "View usage limits"),
    ("provider.connect", r#""none""#, "Connect integration"),
    ("agent.list", r#""<leader>a""#, "List agents"),
    ("agent.cycle", r#""shift+tab""#, "Next agent"),
    ("agent.cycle.reverse", r#""none""#, "Previous agent"),
    ("variant.cycle", r#""ctrl+t""#, "Cycle model variants"),
    ("variant.list", r#""none""#, "List model variants"),
    ("session.page.up", r#""pageup,ctrl+alt+b,kppageup""#, "Scroll messages up by one page"),
    ("session.page.down", r#""pagedown,ctrl+alt+f,kppagedown""#, "Scroll messages down by one page"),
    ("session.line.up", r#""ctrl+alt+y""#, "Scroll messages up by one line"),
    ("session.line.down", r#""ctrl+alt+e""#, "Scroll messages down by one line"),
    ("session.half.page.up", r#""ctrl+alt+u""#, "Scroll messages up by half page"),
    ("session.half.page.down", r#""ctrl+alt+d""#, "Scroll messages down by half page"),
    ("session.first", r#""ctrl+g,home,alt+home,kphome""#, "Navigate to first message"),
    ("session.last", r#""ctrl+alt+g,end,kpend""#, "Navigate to last message"),
    ("session.message.next", r#""none""#, "Navigate to next message"),
    ("session.message.previous", r#""none""#, "Navigate to previous message"),
    ("session.message.user.next", r#""none""#, "Navigate to next user message"),
    ("session.message.user.previous", r#""none""#, "Navigate to previous user message"),
    ("session.messages_last_user", r#""alt+end""#, "Navigate to last user message"),
    ("messages.copy", r#""<leader>y""#, "Copy message"),
    ("session.undo", r#""<leader>u""#, "Undo message"),
    ("session.redo", r#""<leader>r""#, "Redo message"),
    ("session.toggle.thinking", r#""none""#, "Toggle thinking blocks visibility"),
    ("prompt.submit", r#""none""#, "Submit prompt"),
    ("prompt.queue", r#""<leader>return""#, "Queue prompt"),
    ("prompt.editor_context.clear", r#""none""#, "Clear editor context"),
    ("prompt.images.view", r#""<leader>i""#, "View image attachments"),
    ("prompt.skills", r#""none""#, "Open skill selector"),
    ("prompt.stash", r#""none""#, "Stash prompt"),
    ("prompt.stash.pop", r#""none""#, "Pop stashed prompt"),
    ("prompt.stash.list", r#""none""#, "List stashed prompts"),
    ("prompt.clear", r#""ctrl+c""#, "Clear input field"),
    ("prompt.paste", r#"{ "key": "ctrl+v", "preventDefault": false }"#, "Paste from clipboard"),
    ("input.submit", r#""return""#, "Submit input"),
    ("input.newline", r#""shift+return,ctrl+return,alt+return,ctrl+j""#, "Insert newline in input"),
    ("input.move.left", r#""left,ctrl+b""#, "Move cursor left in input"),
    ("input.move.right", r#""right,ctrl+f""#, "Move cursor right in input"),
    ("input.move.up", r#""up""#, "Move cursor up in input"),
    ("input.move.down", r#""down""#, "Move cursor down in input"),
    ("input.select.left", r#""shift+left""#, "Select left in input"),
    ("input.select.right", r#""shift+right""#, "Select right in input"),
    ("input.select.up", r#""shift+up""#, "Select up in input"),
    ("input.select.down", r#""shift+down""#, "Select down in input"),
    ("input.line.home", r#""ctrl+a""#, "Move to start of line in input"),
    ("input.line.end", r#""ctrl+e""#, "Move to end of line in input"),
    ("input.select.line.home", r#""ctrl+shift+a""#, "Select to start of line in input"),
    ("input.select.line.end", r#""ctrl+shift+e""#, "Select to end of line in input"),
    ("input.visual.line.home", r#""alt+a""#, "Move to start of visual line in input"),
    ("input.visual.line.end", r#""alt+e""#, "Move to end of visual line in input"),
    ("input.select.visual.line.home", r#""alt+shift+a""#, "Select to start of visual line in input"),
    ("input.select.visual.line.end", r#""alt+shift+e""#, "Select to end of visual line in input"),
    ("input.buffer.home", r#""none""#, "Move to start of buffer in input"),
    ("input.buffer.end", r#""none""#, "Move to end of buffer in input"),
    ("input.select.buffer.home", r#""shift+home""#, "Select to start of buffer in input"),
    ("input.select.buffer.end", r#""shift+end""#, "Select to end of buffer in input"),
    ("input.delete.line", r#""ctrl+shift+d""#, "Delete line in input"),
    ("input.delete.to.line.end", r#""ctrl+k""#, "Delete to end of line in input"),
    ("input.delete.to.line.start", r#""ctrl+u""#, "Delete to start of line in input"),
    ("input.backspace", r#""backspace,shift+backspace""#, "Backspace in input"),
    ("input.delete", r#""ctrl+d,delete,shift+delete""#, "Delete character in input"),
    ("input.undo", r#""ctrl+-,super+z""#, "Undo in input"),
    ("input.redo", r#""ctrl+.,super+shift+z""#, "Redo in input"),
    ("input.word.forward", r#""alt+f,alt+right,ctrl+right""#, "Move word forward in input"),
    ("input.word.backward", r#""alt+b,alt+left,ctrl+left""#, "Move word backward in input"),
    ("input.select.word.forward", r#""alt+shift+f,alt+shift+right""#, "Select word forward in input"),
    ("input.select.word.backward", r#""alt+shift+b,alt+shift+left""#, "Select word backward in input"),
    ("input.delete.word.forward", r#""alt+d,alt+delete,ctrl+delete""#, "Delete word forward in input"),
    ("input.delete.word.backward", r#""ctrl+w,ctrl+backspace,alt+backspace""#, "Delete word backward in input"),
    ("input.select.all", r#""super+a""#, "Select all in input"),
    ("prompt.history.previous", r#""up""#, "Previous history item"),
    ("prompt.history.next", r#""down""#, "Next history item"),
    ("composer.subagent.up", r#""up""#, "Previous subagent"),
    ("composer.subagent.down", r#""down""#, "Next subagent"),
    ("composer.subagent.select", r#""return""#, "Navigate to subagent"),
    ("composer.subagent.interrupt", r#""ctrl+d""#, "Interrupt subagent"),
    ("composer.shell.up", r#""up""#, "Previous shell"),
    ("composer.shell.down", r#""down""#, "Next shell"),
    ("composer.shell.select", r#""return""#, "View shell output"),
    ("composer.shell.kill", r#""ctrl+d""#, "Kill shell command"),
    ("composer.terminal.up", r#""up,k""#, "Previous terminal"),
    ("composer.terminal.down", r#""down,j""#, "Next terminal"),
    ("composer.terminal.select", r#""return""#, "Select terminal"),
    ("dialog.select.prev", r#""up,ctrl+p""#, "Move to previous dialog item"),
    ("dialog.select.next", r#""down,ctrl+n""#, "Move to next dialog item"),
    ("dialog.select.page_up", r#""pageup""#, "Move up one page in dialog"),
    ("dialog.select.page_down", r#""pagedown""#, "Move down one page in dialog"),
    ("dialog.select.home", r#""home""#, "Move to first dialog item"),
    ("dialog.select.end", r#""end""#, "Move to last dialog item"),
    ("dialog.select.submit", r#""return""#, "Submit selected dialog item"),
    ("dialog.prompt.submit", r#""return""#, "Submit dialog prompt"),
    ("dialog.integration.rename", r#""ctrl+r""#, "Rename integration account"),
    ("dialog.integration.delete", r#""ctrl+d""#, "Delete integration account"),
    ("dialog.integration.copy", r#""tab""#, "Copy authorization link"),
    ("dialog.worktree.generate", r#""tab""#, "Generate worktree name"),
    ("dialog.move_session.new", r#""ctrl+a""#, "New worktree"),
    ("dialog.move_session.move", r#""ctrl+m""#, "Move session to worktree"),
    ("dialog.move_session.delete", r#""ctrl+d""#, "Delete worktree"),
    ("dialog.move_session.refresh", r#""ctrl+r""#, "Refresh worktrees"),
    ("prompt.autocomplete.prev", r#""up,ctrl+p""#, "Move to previous autocomplete item"),
    ("prompt.autocomplete.next", r#""down,ctrl+n""#, "Move to next autocomplete item"),
    ("prompt.autocomplete.hide", r#""escape""#, "Hide autocomplete"),
    ("prompt.autocomplete.select", r#""return""#, "Select autocomplete item"),
    ("prompt.autocomplete.complete", r#""tab""#, "Complete autocomplete item"),
    ("permission.prompt.fullscreen", r#""ctrl+f""#, "Toggle permission prompt fullscreen"),
    ("plugins.toggle", r#""return""#, "Toggle plugin"),
    ("dialog.mcp.toggle", r#""space""#, "Toggle MCP server"),
    ("dialog.plugins.error", r#""space""#, "View plugin error"),
    ("dialog.plugins.update", r#""ctrl+u""#, "Update plugin from plugin dialog"),
    ("dialog.plugins.check", r#""ctrl+r""#, "Check for plugin updates from plugin dialog"),
    ("terminal.suspend", r#""ctrl+z""#, "Suspend terminal"),
    ("terminal.title.toggle", r#""none""#, "Toggle terminal title"),
    ("plugins.list", r#""none""#, "Open plugin manager dialog"),
    ("which-key.toggle", r#""ctrl+alt+k""#, "Toggle which-key panel"),
    ("which-key.layout.toggle", r#""ctrl+alt+shift+k""#, "Switch which-key layout"),
    ("which-key.pending.toggle", r#""ctrl+alt+shift+p""#, "Toggle which-key pending preview"),
    ("which-key.group.previous", r#""ctrl+alt+left,ctrl+alt+[""#, "Previous which-key group"),
    ("which-key.group.next", r#""ctrl+alt+right,ctrl+alt+]""#, "Next which-key group"),
    ("which-key.scroll.up", r#""ctrl+alt+up,ctrl+alt+p""#, "Scroll which-key up"),
    ("which-key.scroll.down", r#""ctrl+alt+down,ctrl+alt+n""#, "Scroll which-key down"),
    ("which-key.page.up", r#""ctrl+alt+pageup""#, "Page which-key up"),
    ("which-key.page.down", r#""ctrl+alt+pagedown""#, "Page which-key down"),
    ("which-key.home", r#""ctrl+alt+home""#, "Jump to first which-key binding"),
    ("which-key.end", r#""ctrl+alt+end""#, "Jump to last which-key binding"),
];

/// `Definitions[name].default`, decoded.
pub fn default_value(name: &str) -> Option<Json> {
    DEFAULTS.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone())
}

static DEFAULTS: LazyLock<Vec<(&'static str, Json)>> = LazyLock::new(|| {
    DEFINITIONS
        .iter()
        .map(|(name, default, _)| (*name, opencode_protocol::wire::json::parse(default).expect("default keybind JSON")))
        .collect()
});

/// `Descriptions[name]`.
pub fn description(name: &str) -> Option<&'static str> {
    DEFINITIONS.iter().find(|(n, _, _)| *n == name).map(|(_, _, d)| *d)
}

/// `unknownKeys(input)`: the override names that are not keybinds, in property order.
pub fn unknown_keys(input: &Object) -> Vec<String> {
    crate::theme::js::keys(input)
        .into_iter()
        .filter(|key| !DEFINITIONS.iter().any(|(name, _, _)| name == key))
        .map(str::to_owned)
        .collect()
}

/// `parse(keybinds)`: every keybind's binding value (the override, else the default), decoded, in definition order.
pub fn parse(keybinds: &Object) -> Result<Vec<(String, Json)>, String> {
    let invalid = unknown_keys(keybinds);
    if !invalid.is_empty() {
        return Err(format!("Unrecognized keybind{}: {}", if invalid.len() == 1 { "" } else { "s" }, invalid.join(", ")));
    }
    let mut out = Vec::new();
    for (name, default) in DEFAULTS.iter() {
        let value = match keybinds.get(*name) {
            Some(Json::Null) | None => default,
            Some(value) => value,
        };
        let decoded = decode_binding_value(value).ok_or_else(|| format!("Invalid binding value for \"{name}\""))?;
        out.push((name.to_string(), decoded));
    }
    Ok(out)
}

/// A binding (`Binding` of `@opentui/keymap`): `key` (a string or a keystroke object), `cmd`, the TUI's `desc`, and
/// whatever else the binding object carried (`event`, `preventDefault`, `fallthrough`, extra fields), in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding(pub Object);

impl Binding {
    pub fn key(&self) -> &Json {
        &self.0["key"]
    }

    pub fn cmd(&self) -> &str {
        match self.0.get("cmd") {
            Some(Json::String(cmd)) => cmd,
            _ => "",
        }
    }

    pub fn desc(&self) -> Option<&str> {
        match self.0.get("desc") {
            Some(Json::String(desc)) => Some(desc),
            _ => None,
        }
    }

    pub fn get(&self, field: &str) -> Option<&Json> {
        self.0.get(field)
    }
}

fn is_key_like(value: &Json) -> bool {
    matches!(value, Json::String(_) | Json::Object(_))
}

fn invalid_binding_value(command: &str, index: Option<usize>) -> String {
    let location = match index {
        None => format!("\"{command}\""),
        Some(i) => format!("\"{command}\" at index {i}"),
    };
    format!("Invalid binding value for {location}: expected false, a key, a binding object, or an array of keys/binding objects")
}

fn resolve_binding_item(command: &str, item: &Json, index: Option<usize>) -> Result<Binding, String> {
    if !is_key_like(item) {
        return Err(invalid_binding_value(command, index));
    }
    let object = match item {
        Json::Object(o) if o.contains_key("key") => o,
        _ => {
            let mut out = Object::new();
            out.insert("key".into(), item.clone());
            out.insert("cmd".into(), Json::String(command.to_owned()));
            return Ok(Binding(out));
        }
    };
    if !is_key_like(&object["key"]) {
        return Err(invalid_binding_value(command, index));
    }
    let mut out = object.clone();
    out.insert("cmd".into(), Json::String(command.to_owned()));
    Ok(Binding(out))
}

/// `bindingDefaults()`: a binding without `desc` gets the keybind's description (`{ desc, ...binding }`).
fn with_binding_defaults(command: &str, binding: Binding) -> Binding {
    if binding.0.contains_key("desc") {
        return binding;
    }
    let mut out = Object::new();
    match description(command) {
        Some(desc) => out.insert("desc".into(), Json::String(desc.to_owned())),
        // `{ desc: undefined }`: no property once serialized, still an own key in JS.
        None => None,
    };
    for (k, v) in binding.0 {
        out.insert(k, v);
    }
    Binding(out)
}

fn resolve_binding_value(command: &str, value: &Json) -> Result<Option<Vec<Binding>>, String> {
    match value {
        Json::Bool(false) => Ok(None),
        Json::String(s) if s == "none" => Ok(None),
        Json::Array(items) if items.is_empty() => Ok(None),
        Json::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, item)| resolve_binding_item(command, item, Some(i)).map(|b| with_binding_defaults(command, b)))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        _ => resolve_binding_item(command, value, None).map(|b| Some(vec![with_binding_defaults(command, b)])),
    }
}

/// `createBindingLookup(config, { bindingDefaults })` with the TUI's binding defaults.
#[derive(Clone, Debug, Default)]
pub struct BindingLookup {
    config: Vec<(String, Json)>,
    by_command: Vec<(String, Vec<Binding>)>,
    bindings: Vec<Binding>,
    gathered: std::cell::RefCell<Vec<(String, Gathered)>>,
}

#[derive(Clone, Debug, Default)]
struct Gathered {
    bindings: Vec<Binding>,
    by_command: Vec<(String, Vec<Binding>)>,
}

impl BindingLookup {
    pub fn new(config: Vec<(String, Json)>) -> Result<BindingLookup, String> {
        let mut lookup = BindingLookup { config, ..Default::default() };
        lookup.normalize()?;
        Ok(lookup)
    }

    fn normalize(&mut self) -> Result<(), String> {
        let mut by_command: Vec<(String, Vec<Binding>)> = Vec::new();
        for (command, value) in &self.config {
            if command.is_empty() {
                return Err("Invalid binding command: command cannot be empty".into());
            }
            match resolve_binding_value(command, value)? {
                None => by_command.retain(|(c, _)| c != command),
                Some(bindings) => match by_command.iter_mut().find(|(c, _)| c == command) {
                    Some(slot) => slot.1 = bindings,
                    None => by_command.push((command.clone(), bindings)),
                },
            }
        }
        self.bindings = by_command.iter().flat_map(|(_, b)| b.iter().cloned()).collect();
        self.by_command = by_command;
        Ok(())
    }

    /// `bindings`: every binding, by command in config order.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// `get(command)`.
    pub fn get(&self, command: &str) -> &[Binding] {
        self.by_command.iter().find(|(c, _)| c == command).map(|(_, b)| b.as_slice()).unwrap_or(&[])
    }

    /// `has(command)`.
    pub fn has(&self, command: &str) -> bool {
        self.by_command.iter().any(|(c, _)| c == command)
    }

    /// `gather(name, commands)`: the bindings of these commands, in this order; cached under `name` (a later call with
    /// the same name returns the first result).
    pub fn gather(&self, name: &str, commands: &[&str]) -> Vec<Binding> {
        if let Some((_, existing)) = self.gathered.borrow().iter().find(|(n, _)| n == name) {
            return existing.bindings.clone();
        }
        let mut next = Gathered::default();
        for command in commands {
            let Some((_, bindings)) = self.by_command.iter().find(|(c, _)| c == command) else { continue };
            match next.by_command.iter_mut().find(|(c, _)| c == command) {
                Some(slot) => slot.1 = bindings.clone(),
                None => next.by_command.push((command.to_string(), bindings.clone())),
            }
            next.bindings.extend(bindings.iter().cloned());
        }
        let result = next.bindings.clone();
        self.gathered.borrow_mut().push((name.to_owned(), next));
        result
    }

    /// `pick(name, commands)`: of a gathered group, the bindings of these commands.
    pub fn pick(&self, name: &str, commands: &[&str]) -> Vec<Binding> {
        let gathered = self.gathered.borrow();
        let Some((_, group)) = gathered.iter().find(|(n, _)| n == name) else { return Vec::new() };
        let mut result = Vec::new();
        for command in commands {
            if let Some((_, bindings)) = group.by_command.iter().find(|(c, _)| c == command) {
                result.extend(bindings.iter().cloned());
            }
        }
        result
    }

    /// `omit(name, commands)`: of a gathered group, the bindings of the other commands.
    pub fn omit(&self, name: &str, commands: &[&str]) -> Vec<Binding> {
        let gathered = self.gathered.borrow();
        let Some((_, group)) = gathered.iter().find(|(n, _)| n == name) else { return Vec::new() };
        group.bindings.iter().filter(|b| !commands.contains(&b.cmd())).cloned().collect()
    }

    /// `invalidate(name?)`.
    pub fn invalidate(&self, name: Option<&str>) {
        match name {
            None => self.gathered.borrow_mut().clear(),
            Some(name) => self.gathered.borrow_mut().retain(|(n, _)| n != name),
        }
    }

    /// `update(config?)`.
    pub fn update(&mut self, config: Option<Vec<(String, Json)>>) -> Result<(), String> {
        if let Some(config) = config {
            self.config = config;
        }
        self.normalize()?;
        self.gathered.borrow_mut().clear();
        Ok(())
    }
}
