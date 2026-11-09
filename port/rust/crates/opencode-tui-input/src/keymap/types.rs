//! The data the keymap API takes and hands out (`types.ts`): layers, bindings, commands, field values, events,
//! the host, command contexts and results, active keys.
use super::keys::{KeyLike, KeySequencePart, KeyStroke};
use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

/// A `() => boolean` condition (`activeWhen`, the `enabled` field's function form).
pub type Matcher = Rc<dyn Fn() -> bool>;

/// A field value of a layer, binding or command (the JS values the TUI passes).
#[derive(Clone)]
pub enum Field {
    Bool(bool),
    Str(String),
    Num(f64),
    /// A function field (`enabled: () => boolean`).
    Fn(Matcher),
    /// An app object the keymap only carries (`opencode`, `slash`); compared by identity.
    Any(Rc<dyn Any>),
}

impl fmt::Debug for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Field::Bool(value) => write!(f, "{value}"),
            Field::Str(value) => write!(f, "{value:?}"),
            Field::Num(value) => write!(f, "{value}"),
            Field::Fn(_) => f.write_str("<fn>"),
            Field::Any(_) => f.write_str("<object>"),
        }
    }
}

impl Field {
    /// The compiled-data equality of `sameCompiledData`: functions are equal (a kept layer takes the new ones),
    /// objects by identity.
    pub(crate) fn same(&self, other: &Field) -> bool {
        match (self, other) {
            (Field::Bool(a), Field::Bool(b)) => a == b,
            (Field::Str(a), Field::Str(b)) => a == b,
            (Field::Num(a), Field::Num(b)) => same_number(*a, *b),
            (Field::Fn(_), Field::Fn(_)) => true,
            (Field::Any(a), Field::Any(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Field::Str(value) => Some(value),
            _ => None,
        }
    }
}

/// Fields in insertion order (`Object.entries`).
pub type Fields = Vec<(String, Field)>;

pub(crate) fn same_fields(a: &Fields, b: &Fields) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|((ka, va), (kb, vb))| ka == kb && va.same(vb))
}

/// `Object.is` of numbers.
pub(crate) fn same_number(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative())
}

/// A runtime data value (`keymap.setData`, a layer's `require`), compared with `Object.is`.
#[derive(Clone, Debug)]
pub enum Data {
    Str(Rc<str>),
    Bool(bool),
    Num(f64),
}

impl PartialEq for Data {
    fn eq(&self, other: &Data) -> bool {
        match (self, other) {
            (Data::Str(a), Data::Str(b)) => a == b,
            (Data::Bool(a), Data::Bool(b)) => a == b,
            (Data::Num(a), Data::Num(b)) => same_number(*a, *b),
            _ => false,
        }
    }
}

impl From<&str> for Data {
    fn from(value: &str) -> Self {
        Data::Str(value.into())
    }
}

/// Attributes published by field compilers (`desc`, `title`, `group`, `category`), in insertion order.
pub type Attrs = Vec<(String, String)>;

pub fn attr<'a>(attrs: Option<&'a Rc<Attrs>>, name: &str) -> Option<&'a str> {
    attrs?.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

/// `KeymapEvent`: what dispatch reads from and marks on a key event.
pub trait KeymapEvent {
    fn name(&self) -> &str;
    fn ctrl(&self) -> bool;
    fn shift(&self) -> bool;
    fn meta(&self) -> bool;
    fn super_key(&self) -> bool;
    fn hyper(&self) -> bool;
    /// Kitty's base-layout code point (`baseCode`), for the base-layout fallback.
    fn base_code(&self) -> Option<u32> {
        None
    }
    fn prevent_default(&self);
    fn stop_propagation(&self);
    fn default_prevented(&self) -> bool;
    fn propagation_stopped(&self) -> bool;
    fn as_any(&self) -> &dyn Any;
}

impl KeymapEvent for crate::events::KeyEvent {
    fn name(&self) -> &str {
        &self.key().name
    }
    fn ctrl(&self) -> bool {
        self.key().ctrl
    }
    fn shift(&self) -> bool {
        self.key().shift
    }
    fn meta(&self) -> bool {
        self.key().meta
    }
    fn super_key(&self) -> bool {
        self.key().super_ == Some(true)
    }
    fn hyper(&self) -> bool {
        self.key().hyper == Some(true)
    }
    fn base_code(&self) -> Option<u32> {
        self.key().base_code
    }
    fn prevent_default(&self) {
        crate::events::KeyEvent::prevent_default(self);
    }
    fn stop_propagation(&self) {
        crate::events::KeyEvent::stop_propagation(self);
    }
    fn default_prevented(&self) -> bool {
        crate::events::KeyEvent::default_prevented(self)
    }
    fn propagation_stopped(&self) -> bool {
        crate::events::KeyEvent::propagation_stopped(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `HostPlatform`, `HostModifier` support and `HostMetadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostMetadata {
    pub platform: &'static str,
    pub primary_modifier: &'static str,
    pub modifiers: [(&'static str, &'static str); 5],
}

/// `KeymapHost`, inverted for Rust: the host calls [`super::Keymap::handle_key_event`],
/// [`super::Keymap::handle_focus_change`], [`super::Keymap::handle_target_destroyed`] and
/// [`super::Keymap::handle_host_destroyed`] where OpenTUI's host subscribes listeners.
pub trait KeymapHost<T> {
    fn is_destroyed(&self) -> bool;
    fn root_target(&self) -> T;
    /// `getFocusedTarget`: the focused, live target.
    fn focused_target(&self) -> Option<T>;
    fn parent_target(&self, target: &T) -> Option<T>;
    fn is_target_destroyed(&self, target: &T) -> bool;
    /// `createCommandEvent`: the synthetic event of a programmatic command (OpenTUI: a `command` key press).
    fn create_command_event(&self) -> Box<dyn KeymapEvent>;
    fn metadata(&self) -> HostMetadata;
    /// A timer for the timed leader (`setTimeout`); the callback runs on the host's loop.
    fn set_timeout(&self, ms: f64, callback: Box<dyn FnOnce()>) -> u64;
    fn clear_timeout(&self, id: u64);
}

/// `TargetMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetMode {
    /// Active while the target or a descendant is focused.
    FocusWithin,
    /// Active while the target itself is focused.
    Focus,
}

/// `CommandContext`.
pub struct CommandContext<'a, T> {
    pub keymap: &'a super::Keymap<T>,
    pub event: &'a dyn KeymapEvent,
    pub focused: Option<T>,
    pub target: Option<T>,
    pub data: Rc<BTreeMap<String, Data>>,
    pub input: String,
    pub command: Option<Rc<Command<T>>>,
}

/// What a command's `run` returns (`CommandResult`): `false` rejects it, a `RunCommandResult` passes through,
/// anything else handles it.
pub enum CommandResult<T> {
    Handled,
    Rejected,
    Result(RunCommandResult<T>),
}

impl<T> From<bool> for CommandResult<T> {
    fn from(handled: bool) -> Self {
        if handled { CommandResult::Handled } else { CommandResult::Rejected }
    }
}

impl<T> From<()> for CommandResult<T> {
    fn from((): ()) -> Self {
        CommandResult::Handled
    }
}

pub type CommandFn<T> = Rc<dyn Fn(&CommandContext<'_, T>) -> CommandResult<T>>;

/// `RunCommandResult.reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunReason {
    NotFound,
    InvalidArgs,
    Rejected,
    Error,
    Inactive,
    Disabled,
}

impl RunReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RunReason::NotFound => "not-found",
            RunReason::InvalidArgs => "invalid-args",
            RunReason::Rejected => "rejected",
            RunReason::Error => "error",
            RunReason::Inactive => "inactive",
            RunReason::Disabled => "disabled",
        }
    }
}

/// `RunCommandResult`.
pub struct RunCommandResult<T> {
    pub ok: bool,
    pub reason: Option<RunReason>,
    pub command: Option<Rc<Command<T>>>,
}

impl<T> RunCommandResult<T> {
    pub fn ok() -> Self {
        RunCommandResult { ok: true, reason: None, command: None }
    }

    pub fn fail(reason: RunReason) -> Self {
        RunCommandResult { ok: false, reason: Some(reason), command: None }
    }
}

impl<T> Clone for RunCommandResult<T> {
    fn clone(&self) -> Self {
        RunCommandResult { ok: self.ok, reason: self.reason, command: self.command.clone() }
    }
}

/// `Command`: a named handler and its fields.
pub struct Command<T> {
    pub name: String,
    pub run: Option<CommandFn<T>>,
    pub fields: Fields,
}

impl<T> Clone for Command<T> {
    fn clone(&self) -> Self {
        Command { name: self.name.clone(), run: self.run.clone(), fields: self.fields.clone() }
    }
}

impl<T> Command<T> {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|(key, _)| key == name).map(|(_, value)| value)
    }
}

/// `BindingCommand`: a command name or an inline handler.
pub enum BindingCommand<T> {
    Name(String),
    Run(CommandFn<T>),
}

impl<T> Clone for BindingCommand<T> {
    fn clone(&self) -> Self {
        match self {
            BindingCommand::Name(name) => BindingCommand::Name(name.clone()),
            BindingCommand::Run(run) => BindingCommand::Run(run.clone()),
        }
    }
}

impl<T> BindingCommand<T> {
    pub fn name(&self) -> Option<&str> {
        match self {
            BindingCommand::Name(name) => Some(name),
            BindingCommand::Run(_) => None,
        }
    }
}

/// `Binding`.
pub struct Binding<T> {
    pub key: KeyLike,
    pub cmd: Option<BindingCommand<T>>,
    /// `"press"` (the default) or `"release"`.
    pub event: Option<String>,
    pub prevent_default: Option<bool>,
    pub fallthrough: Option<bool>,
    pub fields: Fields,
}

impl<T> Clone for Binding<T> {
    fn clone(&self) -> Self {
        Binding {
            key: self.key.clone(),
            cmd: self.cmd.clone(),
            event: self.event.clone(),
            prevent_default: self.prevent_default,
            fallthrough: self.fallthrough,
            fields: self.fields.clone(),
        }
    }
}

impl<T> Binding<T> {
    pub fn new(key: impl Into<KeyLike>, cmd: Option<BindingCommand<T>>) -> Binding<T> {
        Binding { key: key.into(), cmd, event: None, prevent_default: None, fallthrough: None, fields: Vec::new() }
    }
}

/// `Layer`.
pub struct Layer<T> {
    pub target: Option<T>,
    pub target_mode: Option<TargetMode>,
    pub priority: Option<i32>,
    pub bindings: Vec<Binding<T>>,
    pub commands: Vec<Command<T>>,
    /// Layer fields (`enabled`, `mode`, ...).
    pub fields: Fields,
}

impl<T> Default for Layer<T> {
    fn default() -> Self {
        Layer {
            target: None,
            target_mode: None,
            priority: None,
            bindings: Vec::new(),
            commands: Vec::new(),
            fields: Vec::new(),
        }
    }
}

/// `ActiveBinding`.
pub struct ActiveBinding<T> {
    pub sequence: Vec<KeySequencePart>,
    pub command: Option<BindingCommand<T>>,
    pub command_attrs: Option<Rc<Attrs>>,
    pub attrs: Option<Rc<Attrs>>,
    pub release: bool,
    pub prevent_default: bool,
    pub fallthrough: bool,
}

/// `ActiveKey`.
pub struct ActiveKey<T> {
    pub stroke: KeyStroke,
    pub display: String,
    pub token_name: Option<String>,
    pub continues: bool,
    pub command: Option<BindingCommand<T>>,
    pub binding_attrs: Option<Rc<Attrs>>,
    pub command_attrs: Option<Rc<Attrs>>,
    pub bindings: Option<Vec<ActiveBinding<T>>>,
}

/// `CommandEntry`.
pub struct CommandEntry<T> {
    pub command: Rc<Command<T>>,
    pub bindings: Vec<ActiveBinding<T>>,
}

/// `CommandQuery.visibility`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// Each name's first active, enabled command (the default).
    Reachable,
    /// Every active, enabled command.
    Active,
    /// Every registered command.
    Registered,
}
