//! Keybinds: `context/keymap.tsx` (`Keymap.Provider`, `use`, `createLayer`, `useShortcuts`, `useCommands`,
//! `usePendingSequence`, the input modes) over `opencode-tui-input`'s port of `@opentui/keymap` and its OpenTUI host
//! (`@opentui/keymap/opentui`: the renderer as the host, its focused renderable as the focus target).
//!
//! A layer is `create_layer(move || Layer { .. })`: the closure re-runs in an effect, as `useBindings` does, and the
//! layer leaves with the owner. Commands keep TS's `KeymapCommand` fields; `run` returns false to let the key fall
//! through. Hints come from [`use_shortcuts`] (`formatKeySequence`/`formatCommandBindings` with the TUI's aliases).
//!
//! Divergences: one keymap state version signal is shared by every selector (TS gives each `useKeymapSelector` its
//! own, bumped on mount too). The host's platform metadata reports super and hyper as unknown (the renderer does
//! not expose the kitty keyboard capability; only `primaryModifier` bindings read it, and the TUI has none).
//! The textarea's keys go through the keymap: `registerManagedTextareaLayer` (`opencode_tui_core::
//! edit_buffer_bindings`) with the `input.*` keybinds, enabled while a textarea is the focused editor.
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::edit_buffer_bindings;
use opencode_tui_core::input::{KeyEvent, KeyEventType, ParsedKey};
use opencode_tui_core::renderables::textarea::TextareaAction;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderer;
use opencode_tui_core::tree::{self, Element};
use opencode_tui_input::keymap::{self as km, KeymapEvent as _};

use crate::config::keybind;
use crate::ctx::Tui;

const MODE_KEY: &str = "opencode.mode";
const MODE_BASE: &str = "base";

/// `COMMAND_PALETTE_COMMAND`.
pub const COMMAND_PALETTE_COMMAND: &str = "command.palette.show";

/// `boolean | (() => boolean)` (`enabled`, `suggested`).
#[derive(Clone)]
pub enum Enabled {
    Bool(bool),
    Fn(Rc<dyn Fn() -> bool>),
}

impl Enabled {
    pub fn get(&self) -> bool {
        match self {
            Enabled::Bool(value) => *value,
            Enabled::Fn(f) => f(),
        }
    }

    fn same(&self, other: &Enabled) -> bool {
        match (self, other) {
            (Enabled::Bool(a), Enabled::Bool(b)) => a == b,
            (Enabled::Fn(_), Enabled::Fn(_)) => true,
            _ => false,
        }
    }
}

impl From<bool> for Enabled {
    fn from(value: bool) -> Self {
        Enabled::Bool(value)
    }
}

/// A command's `bind`: the configured keybind of its id (TS `undefined`), none (`false`), or a key.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Bind {
    #[default]
    Config,
    Off,
    Key(String),
}

/// `slash` of a command (prompt slash completion).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slash {
    pub name: String,
    pub aliases: Vec<String>,
    /// Keeps the slash command in the prompt and passes its raw input to `run`.
    pub arguments: bool,
}

/// `KeymapCommand`.
#[derive(Clone)]
pub struct Command {
    /// The command and config keybind id; `None` for an inline command (which needs `bind`).
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub group: Option<String>,
    pub enabled: Option<Enabled>,
    pub bind: Bind,
    /// Listed in the command palette.
    pub palette: bool,
    pub slash: Option<Slash>,
    pub suggested: Option<Enabled>,
    /// `run(input)`: false lets the key continue to the next binding or the focused renderable.
    pub run: Rc<dyn Fn(Option<String>) -> bool>,
}

impl Default for Command {
    fn default() -> Self {
        Command {
            id: None,
            title: None,
            description: None,
            group: None,
            enabled: None,
            bind: Bind::Config,
            palette: false,
            slash: None,
            suggested: None,
            run: Rc::new(|_| true),
        }
    }
}

impl Command {
    /// A named command whose `run` always handles the key.
    pub fn new(id: &str, run: impl Fn() + 'static) -> Command {
        Command { id: Some(id.to_string()), run: Rc::new(move |_| { run(); true }), ..Command::default() }
    }

    /// A named command that gets the dispatch input and decides whether it handled the key.
    pub fn with_input(id: &str, run: impl Fn(Option<String>) -> bool + 'static) -> Command {
        Command { id: Some(id.to_string()), run: Rc::new(run), ..Command::default() }
    }

    /// An inline command (`{ bind, run }`).
    pub fn inline(bind: &str, run: impl Fn() -> bool + 'static) -> Command {
        Command { bind: Bind::Key(bind.to_string()), run: Rc::new(move |_| run()), ..Command::default() }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }
    pub fn enabled(mut self, enabled: impl Into<Enabled>) -> Self {
        self.enabled = Some(enabled.into());
        self
    }
    pub fn enabled_fn(mut self, enabled: impl Fn() -> bool + 'static) -> Self {
        self.enabled = Some(Enabled::Fn(Rc::new(enabled)));
        self
    }
    pub fn bind(mut self, bind: Bind) -> Self {
        self.bind = bind;
        self
    }
    pub fn palette(mut self, palette: bool) -> Self {
        self.palette = palette;
        self
    }
    pub fn slash(mut self, name: &str, aliases: &[&str]) -> Self {
        self.slash = Some(Slash { name: name.into(), aliases: aliases.iter().map(|a| a.to_string()).collect(), arguments: false });
        self
    }
    pub fn slash_arguments(mut self, name: &str) -> Self {
        self.slash = Some(Slash { name: name.into(), aliases: Vec::new(), arguments: true });
        self
    }
    pub fn suggested(mut self, suggested: impl Into<Enabled>) -> Self {
        self.suggested = Some(suggested.into());
        self
    }

    /// `isSuggestedPaletteCommand`.
    pub fn is_suggested(&self) -> bool {
        self.suggested.as_ref().is_some_and(Enabled::get)
    }

    /// Runs the command as `useCommands()` hands it out (a reachable command dispatched by id).
    pub fn invoke(&self, input: Option<String>) {
        (self.run)(input);
    }

    // `sameData`: data fields only; functions are new closures on every run.
    fn same_data(&self, other: &Command) -> bool {
        let same_opt = |a: &Option<Enabled>, b: &Option<Enabled>| match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same(b),
            _ => false,
        };
        self.id == other.id
            && self.title == other.title
            && self.description == other.description
            && self.group == other.group
            && self.bind == other.bind
            && self.palette == other.palette
            && self.slash == other.slash
            && same_opt(&self.enabled, &other.enabled)
            && same_opt(&self.suggested, &other.suggested)
    }
}

/// `KeymapLayer`.
#[derive(Default)]
pub struct Layer {
    /// One OpenCode input mode (`"modal"`, `"autocomplete"`, ...); `"global"` opts out; `None` is the base mode.
    pub mode: Option<&'static str>,
    pub enabled: Option<Enabled>,
    /// Limits the layer to a focused renderable (resolved in the layer's effect, so it may read signals).
    pub target: Option<Rc<dyn Fn() -> Option<Element>>>,
    /// `targetMode`: focus-within (the default with a target) or the target itself.
    pub target_focus_only: bool,
    pub priority: Option<i32>,
    pub commands: Vec<Command>,
    /// Ids of commands whose configured bindings apply in this layer.
    pub bindings: Vec<&'static str>,
}

/// What a layer's engine command carries back to `useCommands` (TS: the command's `opencode` object).
struct Kept(RefCell<Command>);

/// The provider's value (`Context`).
struct Value {
    keymap: km::Keymap<Element>,
    config: Rc<crate::config::Resolved>,
    invocation: RefCell<Option<(String, Option<String>)>>,
    /// Bumped on every keymap state change (`useKeymapStateVersion`).
    version: Signal<u64>,
    mode: Mode,
    leader: Option<km::KeyLike>,
}

impl Value {
    fn input(&self, id: &str) -> Option<String> {
        self.invocation.borrow().as_ref().filter(|(current, _)| current == id).and_then(|(_, input)| input.clone())
    }
}

thread_local! {
    static VALUE: RefCell<Option<Rc<Value>>> = const { RefCell::new(None) };
}

fn try_value() -> Option<Rc<Value>> {
    VALUE.with(|v| v.borrow().clone())
}

/// The OpenTUI host: the renderer's root, focus and timers.
struct Host {
    root: Element,
}

impl km::KeymapHost<Element> for Host {
    fn is_destroyed(&self) -> bool {
        renderer::is_destroyed()
    }
    fn root_target(&self) -> Element {
        self.root
    }
    fn focused_target(&self) -> Option<Element> {
        renderer::current_focused_renderable().filter(|f| f.is_alive() && !f.is_destroyed() && f.focused())
    }
    fn parent_target(&self, target: &Element) -> Option<Element> {
        target.parent()
    }
    fn is_target_destroyed(&self, target: &Element) -> bool {
        !target.is_alive() || target.is_destroyed()
    }
    fn create_command_event(&self) -> Box<dyn km::KeymapEvent> {
        Box::new(Event::new(ParsedKey { name: "command".into(), source: "raw", ..ParsedKey::default() }, false, false))
    }
    fn metadata(&self) -> km::HostMetadata {
        let macos = cfg!(target_os = "macos");
        km::HostMetadata {
            platform: if macos { "macos" } else { "linux" },
            primary_modifier: if macos { "super" } else { "ctrl" },
            modifiers: [
                ("ctrl", "supported"),
                ("shift", "supported"),
                ("meta", "supported"),
                ("super", "unknown"),
                ("hyper", "unknown"),
            ],
        }
    }
    fn set_timeout(&self, ms: f64, callback: Box<dyn FnOnce()>) -> u64 {
        renderer::set_timeout(ms.max(0.0) as u64, callback)
    }
    fn clear_timeout(&self, id: u64) {
        renderer::clear_timeout(id);
    }
}

/// A key event as the keymap marks it (the renderer's `KeyEvent` takes `&mut` to mark).
struct Event {
    key: ParsedKey,
    prevented: Cell<bool>,
    stopped: Cell<bool>,
}

impl Event {
    fn new(key: ParsedKey, prevented: bool, stopped: bool) -> Event {
        Event { key, prevented: Cell::new(prevented), stopped: Cell::new(stopped) }
    }
}

impl km::KeymapEvent for Event {
    fn name(&self) -> &str {
        &self.key.name
    }
    fn ctrl(&self) -> bool {
        self.key.ctrl
    }
    fn shift(&self) -> bool {
        self.key.shift
    }
    fn meta(&self) -> bool {
        self.key.meta
    }
    fn super_key(&self) -> bool {
        self.key.super_
    }
    fn hyper(&self) -> bool {
        self.key.hyper
    }
    fn base_code(&self) -> Option<u32> {
        self.key.base_code
    }
    fn prevent_default(&self) {
        self.prevented.set(true);
    }
    fn stop_propagation(&self) {
        self.stopped.set(true);
    }
    fn default_prevented(&self) -> bool {
        self.prevented.get()
    }
    fn propagation_stopped(&self) -> bool {
        self.stopped.get()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The key an intercept or command sees (`event.sequence`, `event.option`, ...).
pub fn parsed_key(event: &dyn km::KeymapEvent) -> Option<&ParsedKey> {
    event.as_any().downcast_ref::<Event>().map(|e| &e.key)
}

/// `createMode(keymap)`: a stack of modes, the newest enabled one current, published as the `opencode.mode` data
/// that layers with a `mode` require.
struct Mode {
    stack: Signal<Vec<Rc<ModeEntry>>>,
    current: Memo<String>,
    next: Cell<u64>,
}

struct ModeEntry {
    id: u64,
    mode: String,
    enabled: Rc<dyn Fn() -> bool>,
}

fn create_mode(keymap: &km::Keymap<Element>) -> (Mode, km::Dispose) {
    let stack: Signal<Vec<Rc<ModeEntry>>> = create_signal_with(Vec::new(), None).0;
    let current = create_memo(move || {
        stack.with(|items| items.iter().rev().find(|item| (item.enabled)()).map(|item| item.mode.clone()))
            .unwrap_or_else(|| MODE_BASE.to_string())
    });
    // Publish mode changes before another command can be dispatched in the same callback.
    {
        let keymap = keymap.clone();
        create_computed(move || keymap.set_data(MODE_KEY, Some(km::Data::from(current.get().as_str()))));
    }
    let unregister = keymap.register_layer_fields(vec![(
        "mode",
        Rc::new(|value: &km::Field, context: &mut km::FieldContext<'_>| {
            let mode = value.as_str().unwrap_or_default();
            context.require(MODE_KEY, km::Data::from(mode))
        }) as km::FieldCompiler,
    )]);
    let keymap = keymap.clone();
    let dispose: km::Dispose = Box::new(move || {
        stack.set(Vec::new());
        unregister();
        keymap.set_data(MODE_KEY, None);
    });
    (Mode { stack, current, next: Cell::new(0) }, dispose)
}

/// `formatOptions(config)`.
fn format_options(v: &Value) -> km::format::FormatOptions {
    let leader = match &v.leader {
        Some(km::KeyLike::Str(text)) => text.clone(),
        Some(km::KeyLike::Stroke(stroke)) => km::normalize_key_stroke(stroke).map(|s| km::stringify_key_stroke(&s)).unwrap_or_default(),
        None => keybind::LEADER_DEFAULT.to_string(),
    };
    let pairs = |items: &[(&str, &str)]| items.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
    km::format::FormatOptions {
        token_display: vec![("leader".to_string(), leader)],
        key_name_aliases: Some(pairs(&[
            ("up", "↑"),
            ("down", "↓"),
            ("left", "←"),
            ("right", "→"),
            ("pageup", "pgup"),
            ("pagedown", "pgdn"),
            ("delete", "del"),
        ])),
        modifier_aliases: Some(pairs(&[("meta", "alt")])),
        separator: None,
    }
}

/// A config binding (`config.keybinds.get(id)`) as the keymap's binding.
fn config_binding(binding: &keybind::Binding) -> Option<km::Binding<Element>> {
    let key = match binding.key() {
        Json::String(text) => km::KeyLike::Str(text.clone()),
        Json::Object(object) => {
            let flag = |name: &str| match object.get(name) {
                Some(Json::Bool(value)) => Some(*value),
                _ => None,
            };
            let name = match object.get("name") {
                Some(Json::String(name)) => name.clone(),
                _ => return None,
            };
            km::KeyLike::Stroke(km::KeyStrokeInput {
                name,
                ctrl: flag("ctrl"),
                shift: flag("shift"),
                meta: flag("meta"),
                super_: flag("super"),
                hyper: flag("hyper"),
            })
        }
        _ => return None,
    };
    let mut out = km::Binding::new(key, Some(km::BindingCommand::Name(binding.cmd().to_string())));
    for (name, value) in binding.0.iter() {
        match (name.as_str(), value) {
            ("key" | "cmd", _) => {}
            ("event", Json::String(event)) => out.event = Some(event.clone()),
            ("preventDefault", Json::Bool(value)) => out.prevent_default = Some(*value),
            ("fallthrough", Json::Bool(value)) => out.fallthrough = Some(*value),
            (_, Json::String(text)) => out.fields.push((name.clone(), km::Field::Str(text.clone()))),
            (_, Json::Bool(flag)) => out.fields.push((name.clone(), km::Field::Bool(*flag))),
            (_, Json::Number(n)) => out.fields.push((name.clone(), km::Field::Num(*n))),
            _ => {}
        }
    }
    Some(out)
}

fn config_bindings(v: &Value, id: &str) -> Vec<km::Binding<Element>> {
    v.config.keybinds.get(id).iter().filter_map(config_binding).collect()
}

/// `Keymap.Provider`: the keymap with the TUI's addons, its key and focus wiring, and the context.
pub fn install(t: &Tui) {
    let root = tree::root().expect("the renderer's root");
    let keymap = km::Keymap::create_default(Rc::new(Host { root }));
    let (mode, mode_dispose) = create_mode(&keymap);
    let leader = t.config.keybinds.get("leader").first().and_then(|binding| match binding.key() {
        Json::String(text) => Some(km::KeyLike::Str(text.clone())),
        _ => config_binding(binding).map(|b| b.key),
    });
    let mut dispose: Vec<km::Dispose> = vec![
        km::addons::register_comma_bindings(&keymap),
        keymap.append_binding_expander(Rc::new(|context: &km::ExpanderContext<'_>| {
            let key = expand_aliases(context.input);
            if key == context.input {
                return Ok(None);
            }
            Ok(Some(vec![km::Expansion { key, displays: context.displays.map(|d| d.to_vec()) }]))
        })),
        km::addons::register_base_layout_fallback(&keymap),
        km::addons::register_escape_clears_pending_sequence(&keymap, true, 0),
        km::addons::register_backspace_pops_pending_sequence(&keymap, true, 0),
        edit_buffer_bindings::register_managed_textarea_layer(
            &keymap,
            km::Layer {
                fields: vec![(
                    "enabled".into(),
                    km::Field::Fn(Rc::new(|| renderer::current_focused_editor().is_some())),
                )],
                bindings: TextareaAction::ALL
                    .iter()
                    .flat_map(|&action| t.config.keybinds.get(edit_buffer_bindings::edit_buffer_command_name(action)))
                    .filter_map(config_binding)
                    .collect(),
                ..km::Layer::default()
            },
        ),
    ];
    if let Some(trigger) = &leader {
        if let Ok(off) = km::addons::register_timed_leader(&keymap, trigger, Some("leader"), Some(t.config.leader.timeout)) {
            dispose.push(off);
        }
    }
    let version = create_signal(0u64).0;
    let value = Rc::new(Value {
        keymap: keymap.clone(),
        config: t.config.clone(),
        invocation: RefCell::new(None),
        version,
        mode,
        leader,
    });
    VALUE.with(|v| *v.borrow_mut() = Some(value.clone()));
    dispose.push(keymap.on_state(Rc::new(move || version.set_with(|v| v + 1))));
    // The host's key listener, prepended to the renderer's.
    let listener = {
        let keymap = keymap.clone();
        renderer::prepend_on_key(move |key: &mut KeyEvent| {
            let event = Event::new(key.key.clone(), key.default_prevented, key.propagation_stopped);
            keymap.handle_key_event(&event, key.event_type == KeyEventType::Release);
            if event.default_prevented() {
                key.prevent_default();
            }
            if event.propagation_stopped() {
                key.stop_propagation();
            }
        })
    };
    let focus = {
        let keymap = keymap.clone();
        renderer::on_focused_renderable(move || keymap.handle_focus_change())
    };
    on_cleanup(move || {
        renderer::off_key(listener);
        renderer::off_focused_renderable(focus);
        for item in dispose.into_iter().rev() {
            item();
        }
        mode_dispose();
        VALUE.with(|v| *v.borrow_mut() = None);
    });
}

/// The Provider's binding expander: `enter`, `esc`, `pgdown`, `pgup` as the key names they alias.
fn expand_aliases(input: &str) -> String {
    let mut out = input.to_string();
    for (alias, name) in [("enter", "return"), ("esc", "escape"), ("pgdown", "pagedown"), ("pgup", "pageup")] {
        out = replace_alias(&out, alias, name);
    }
    out
}

// `result.replace(new RegExp(`(^|[+,\\s>])${alias}(?=$|[+,\\s<])`, "gi"), `$1${value}`)`.
fn replace_alias(input: &str, alias: &str, name: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let alias_chars: Vec<char> = alias.chars().collect();
    let before = |c: char| c == '+' || c == ',' || c == '>' || c.is_whitespace();
    let after = |c: char| c == '+' || c == ',' || c == '<' || c.is_whitespace();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let starts = i == 0 || before(chars[i - 1]);
        let matches = starts
            && i + alias_chars.len() <= chars.len()
            && chars[i..i + alias_chars.len()].iter().zip(&alias_chars).all(|(a, b)| a.eq_ignore_ascii_case(b))
            && (i + alias_chars.len() == chars.len() || after(chars[i + alias_chars.len()]));
        if matches {
            out.push_str(name);
            i += alias_chars.len();
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `Keymap.use().dispatch(id, input)`: runs a reachable command by id.
pub fn dispatch(id: &str, input: Option<String>) {
    let Some(v) = try_value() else { return };
    let previous = v.invocation.replace(Some((id.to_string(), input)));
    v.keymap.dispatch_command(id, None, false);
    *v.invocation.borrow_mut() = previous;
}

/// `Keymap.use().mode.push(mode, active)`: pushes a mode until the returned pop; it claims input only while active
/// (and the caller's subtree is interactive).
pub fn mode_push(mode: &str, active: Option<Rc<dyn Fn() -> bool>>) -> Box<dyn FnOnce()> {
    let Some(v) = try_value() else { return Box::new(|| {}) };
    let interactive = crate::interactivity::use_interactivity();
    let enabled: Rc<dyn Fn() -> bool> = match active {
        Some(active) => Rc::new(move || interactive() && active()),
        None => interactive,
    };
    let id = v.mode.next.get() + 1;
    v.mode.next.set(id);
    let entry = Rc::new(ModeEntry { id, mode: mode.to_string(), enabled });
    // Inactive scopes retain their stack position beneath any newer modes.
    v.mode.stack.update(|items| items.push(entry));
    let stack = v.mode.stack;
    Box::new(move || {
        if stack.is_alive() {
            stack.update(|items| items.retain(|item| item.id != id));
        }
    })
}

/// `Keymap.use().mode.current()`.
pub fn mode_current() -> String {
    try_value().map(|v| v.mode.current.get()).unwrap_or_else(|| MODE_BASE.to_string())
}

/// `Keymap.use().isLeader(event)`.
pub fn is_leader(event: &KeyEvent) -> bool {
    let Some(v) = try_value() else { return false };
    let Some(leader) = &v.leader else { return false };
    let Ok(matcher) = v.keymap.create_key_matcher(leader) else { return false };
    matcher.matches_event(&Event::new(event.key.clone(), false, false)).unwrap_or(false)
}

/// `Keymap.use().intercept("key", ..)`: a key hook before the layers (higher priority first).
pub fn intercept_key(priority: i32, listener: impl Fn(&dyn km::KeymapEvent) -> bool + 'static) -> km::Dispose {
    let Some(v) = try_value() else { return Box::new(|| {}) };
    v.keymap.intercept_key(
        Rc::new(move |context: &km::KeyInputContext<'_>| {
            if listener(context.event) {
                context.consume(true, true);
            }
        }),
        priority,
        false,
    )
}

/// `Keymap.createLayer(() => layer)`.
pub fn create_layer(input: impl Fn() -> Layer + 'static) {
    let Some(v) = try_value() else { return };
    let interactive = crate::interactivity::use_interactivity();
    // The keymap rebuilds a re-registered layer when a command's object is new, so each command keeps one object
    // while its data is unchanged.
    let stable: RefCell<HashMap<String, Rc<Kept>>> = RefCell::new(HashMap::new());
    let slot = Rc::new(v.keymap.create_layer_slot());
    let watched: Rc<Cell<Option<(Element, u64)>>> = Rc::new(Cell::new(None));
    {
        let v = v.clone();
        let slot = slot.clone();
        let watched = watched.clone();
        on_cleanup(move || {
            if let Some((el, token)) = watched.take() {
                el.off(token);
            }
            v.keymap.replace_layer(&slot, None);
        });
    }
    create_effect(move || {
        let layer = input();
        let mut fields: km::Fields = Vec::new();
        let enabled = if interactive() { layer.enabled.clone() } else { Some(Enabled::Bool(false)) };
        if let Some(enabled) = enabled {
            fields.push(("enabled".into(), enabled_field(enabled)));
        }
        if layer.mode != Some("global") {
            fields.push(("mode".into(), km::Field::Str(layer.mode.unwrap_or(MODE_BASE).to_string())));
        }
        let mut commands = Vec::new();
        let mut inline = Vec::new();
        let mut named_bindings = Vec::new();
        for command in layer.commands {
            let Some(id) = command.id.clone() else {
                if let Bind::Key(key) = &command.bind {
                    inline.push(inline_binding(key, command.clone()));
                }
                continue;
            };
            let kept = {
                let mut stable = stable.borrow_mut();
                match stable.get(&id) {
                    Some(previous) if previous.0.borrow().same_data(&command) => {
                        let mut kept = previous.0.borrow_mut();
                        kept.run = command.run.clone();
                        kept.enabled = command.enabled.clone();
                        kept.suggested = command.suggested.clone();
                        drop(kept);
                        previous.clone()
                    }
                    _ => {
                        let kept = Rc::new(Kept(RefCell::new(command.clone())));
                        stable.insert(id.clone(), kept.clone());
                        kept
                    }
                }
            };
            match &command.bind {
                Bind::Off => {}
                bind => {
                    let configured = config_bindings(&v, &id);
                    if !configured.is_empty() {
                        named_bindings.extend(configured);
                    } else if let Bind::Key(key) = bind {
                        named_bindings.push(km::Binding::new(key.as_str(), Some(km::BindingCommand::Name(id.clone()))));
                    }
                }
            }
            commands.push(engine_command(&v, &id, kept, &command));
        }
        let mut bindings = inline;
        bindings.extend(named_bindings);
        for id in &layer.bindings {
            bindings.extend(config_bindings(&v, id));
        }
        let mut base = km::Layer { priority: layer.priority, bindings, commands, fields, ..km::Layer::default() };
        let Some(target) = layer.target.as_ref() else {
            if let Some((el, token)) = watched.take() {
                el.off(token);
            }
            v.keymap.replace_layer(&slot, Some(base));
            return;
        };
        let target = target();
        let previous = watched.take();
        match (previous, target) {
            (Some((el, token)), Some(next)) if el == next => watched.set(Some((el, token))),
            (previous, next) => {
                if let Some((el, token)) = previous {
                    el.off(token);
                }
                if let Some(next) = next {
                    let keymap = v.keymap.clone();
                    let token = next.on("destroyed", Rc::new(move || keymap.handle_target_destroyed(&next)));
                    watched.set(Some((next, token)));
                }
            }
        }
        match target {
            Some(target) => {
                base.target = Some(target);
                base.target_mode = Some(if layer.target_focus_only { km::TargetMode::Focus } else { km::TargetMode::FocusWithin });
                v.keymap.replace_layer(&slot, Some(base));
            }
            None => v.keymap.replace_layer(&slot, None),
        }
    });
}

fn enabled_field(enabled: Enabled) -> km::Field {
    match enabled {
        Enabled::Bool(value) => km::Field::Bool(value),
        Enabled::Fn(f) => km::Field::Fn(Rc::new(move || f())),
    }
}

fn inline_binding(key: &str, command: Command) -> km::Binding<Element> {
    let desc = command.title.clone().or(command.description.clone());
    let group = command.group.clone();
    let run: km::CommandFn<Element> = Rc::new(move |_context| {
        if let Some(enabled) = &command.enabled {
            if !enabled.get() {
                return km::CommandResult::Rejected;
            }
        }
        (command.run)(None).into()
    });
    let mut binding = km::Binding::new(key, Some(km::BindingCommand::Run(run)));
    if let Some(desc) = desc {
        binding.fields.push(("desc".into(), km::Field::Str(desc)));
    }
    if let Some(group) = group {
        binding.fields.push(("group".into(), km::Field::Str(group)));
    }
    binding
}

fn engine_command(v: &Rc<Value>, id: &str, kept: Rc<Kept>, command: &Command) -> km::Command<Element> {
    let mut fields: km::Fields = Vec::new();
    if let Some(title) = &command.title {
        fields.push(("title".into(), km::Field::Str(title.clone())));
    }
    if let Some(enabled) = &command.enabled {
        let field = match enabled {
            Enabled::Bool(value) => km::Field::Bool(*value),
            Enabled::Fn(_) => {
                let kept = kept.clone();
                km::Field::Fn(Rc::new(move || {
                    let enabled = kept.0.borrow().enabled.clone();
                    enabled.is_none_or(|e| e.get())
                }))
            }
        };
        fields.push(("enabled".into(), field));
    }
    fields.push(("opencode".into(), km::Field::Any(kept.clone() as Rc<dyn Any>)));
    if let Some(description) = &command.description {
        fields.push(("desc".into(), km::Field::Str(description.clone())));
    }
    if let Some(group) = &command.group {
        fields.push(("category".into(), km::Field::Str(group.clone())));
    }
    if command.palette {
        fields.push(("namespace".into(), km::Field::Str("palette".into())));
    }
    let value = Rc::downgrade(v);
    let name = id.to_string();
    km::Command {
        name: id.to_string(),
        run: Some(Rc::new(move |_context| {
            let Some(v) = value.upgrade() else { return km::CommandResult::Rejected };
            // The keymap ends a completed key sequence only after its command returns; a command that changes the
            // view meanwhile would make it revalidate the finished sequence while components are disposed.
            v.keymap.clear_pending_sequence();
            let run = kept.0.borrow().run.clone();
            run(v.input(&name)).into()
        })),
        fields,
    }
}

/// A list of commands, compared by identity (each keymap change makes a new one).
#[derive(Clone, Default)]
pub struct Commands(pub Rc<Vec<Rc<Command>>>);

impl PartialEq for Commands {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl std::ops::Deref for Commands {
    type Target = [Rc<Command>];
    fn deref(&self) -> &[Rc<Command>] {
        &self.0
    }
}

/// `Keymap.useCommands()`: the reachable commands, in the keymap's order; each one's `run` dispatches it.
pub fn use_commands() -> Memo<Commands> {
    let v = try_value();
    create_memo(move || {
        let Some(v) = v.as_ref() else { return Commands::default() };
        v.version.get();
        let commands = v.keymap.get_commands(km::Visibility::Reachable, None);
        let list = commands
            .iter()
            .map(|command| {
                let name = command.name.clone();
                let opencode = command.field("opencode").and_then(|field| match field {
                    km::Field::Any(any) => any.clone().downcast::<Kept>().ok(),
                    _ => None,
                });
                let mut out = match opencode {
                    Some(kept) => kept.0.borrow().clone(),
                    None => {
                        let text = |name: &str| command.field(name).and_then(km::Field::as_str).map(str::to_string);
                        Command {
                            id: Some(name.clone()),
                            title: text("title"),
                            description: text("desc"),
                            group: text("category"),
                            enabled: command.field("enabled").and_then(|field| match field {
                                km::Field::Bool(value) => Some(Enabled::Bool(*value)),
                                km::Field::Fn(f) => Some(Enabled::Fn(f.clone())),
                                _ => None,
                            }),
                            palette: text("namespace").as_deref() == Some("palette"),
                            ..Command::default()
                        }
                    }
                };
                out.run = Rc::new(move |input| {
                    dispatch(&name, input);
                    true
                });
                Rc::new(out)
            })
            .collect();
        Commands(Rc::new(list))
    })
}

#[derive(Clone, Default, PartialEq)]
struct Formatted {
    first: Option<String>,
    all: Option<String>,
    list: Vec<String>,
}

/// `Keymap.useShortcuts()`: the formatted bindings of every registered command.
#[derive(Clone, Copy)]
pub struct Shortcuts(Memo<Rc<HashMap<String, Formatted>>>);

impl Shortcuts {
    /// The first binding (`shortcuts.get(id)`).
    pub fn get(&self, id: &str) -> Option<String> {
        self.0.with(|map| map.get(id).and_then(|f| f.first.clone()))
    }
    /// Every binding, `, `-separated (`shortcuts.all(id)`).
    pub fn all(&self, id: &str) -> Option<String> {
        self.0.with(|map| map.get(id).and_then(|f| f.all.clone()))
    }
    /// Each binding (`shortcuts.list(id)`).
    pub fn list(&self, id: &str) -> Vec<String> {
        self.0.with(|map| map.get(id).map(|f| f.list.clone()).unwrap_or_default())
    }
}

pub fn use_shortcuts() -> Shortcuts {
    let v = try_value();
    Shortcuts(create_memo(move || {
        let Some(v) = v.as_ref() else { return Rc::new(HashMap::new()) };
        v.version.get();
        let options = format_options(v);
        let commands: Vec<String> =
            v.keymap.get_commands(km::Visibility::Registered, None).iter().map(|c| c.name.clone()).collect();
        let bindings = v.keymap.get_command_bindings(km::Visibility::Registered, &commands, None);
        let mut map = HashMap::new();
        for id in commands {
            let current: &[km::ActiveBinding<Element>] =
                bindings.iter().find(|(name, _)| *name == id).map(|(_, b)| b.as_slice()).unwrap_or(&[]);
            let first = km::format::format_key_sequence(current.first().map(|b| b.sequence.as_slice()), &options);
            let all = km::format::format_command_bindings(current.iter().map(|b| b.sequence.as_slice()), &options, None, true);
            let list = current
                .iter()
                .map(|b| km::format::format_key_sequence(Some(&b.sequence), &options))
                .filter(|s| !s.is_empty())
                .collect();
            map.insert(id, Formatted { first: (!first.is_empty()).then_some(first), all, list });
        }
        Rc::new(map)
    }))
}

/// `Keymap.useShortcut(id)`.
pub fn use_shortcut(id: &str) -> impl Fn() -> Option<String> + Clone + 'static {
    let shortcuts = use_shortcuts();
    let id = id.to_string();
    move || shortcuts.get(&id)
}

/// `Keymap.usePendingSequence()`: the pending sequence's parts as (display, token name).
pub fn use_pending_sequence() -> Memo<Vec<(String, Option<String>)>> {
    let v = try_value();
    create_memo(move || {
        let Some(v) = v.as_ref() else { return Vec::new() };
        v.version.get();
        v.keymap.get_pending_sequence().into_iter().map(|part| (part.display, part.token_name)).collect()
    })
}

/// `Keymap.useLeaderActive()`.
pub fn use_leader_active() -> impl Fn() -> bool + Clone + 'static {
    let pending = use_pending_sequence();
    move || pending.with(|parts| parts.first().is_some_and(|(_, token)| token.as_deref() == Some("leader")))
}
