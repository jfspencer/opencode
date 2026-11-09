//! The Rust side of harness/keymap-oracle.ts: replays its programs through `opencode_tui_input::keymap` on a test
//! host with a target tree and fake timers, and writes the answers in the oracle's format.
//!
//! usage: keymap-driver <programs> <results>
use opencode_tui_input::keymap::addons;
use opencode_tui_input::keymap::format::{FormatOptions, format_command_bindings, format_key_sequence};
use opencode_tui_input::keymap::*;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

struct TargetData {
    id: String,
    parent: Option<Target>,
    destroyed: Cell<bool>,
}

#[derive(Clone)]
struct Target(Rc<TargetData>);

impl PartialEq for Target {
    fn eq(&self, other: &Target) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

struct Event {
    name: String,
    mods: [bool; 5],
    base: Option<u32>,
    prevented: Cell<bool>,
    stopped: Cell<bool>,
}

impl KeymapEvent for Event {
    fn name(&self) -> &str {
        &self.name
    }
    fn ctrl(&self) -> bool {
        self.mods[0]
    }
    fn shift(&self) -> bool {
        self.mods[1]
    }
    fn meta(&self) -> bool {
        self.mods[2]
    }
    fn super_key(&self) -> bool {
        self.mods[3]
    }
    fn hyper(&self) -> bool {
        self.mods[4]
    }
    fn base_code(&self) -> Option<u32> {
        self.base
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

fn event(name: &str, mods: &str, base: Option<u32>) -> Event {
    let bits = mods.as_bytes();
    Event {
        name: name.to_owned(),
        mods: [bits[0] == b'1', bits[1] == b'1', bits[2] == b'1', bits[3] == b'1', bits[4] == b'1'],
        base,
        prevented: Cell::new(false),
        stopped: Cell::new(false),
    }
}

type Timers = Rc<RefCell<(f64, u64, Vec<(u64, f64, Option<Box<dyn FnOnce()>>)>)>>;

struct Host {
    root: Target,
    focused: RefCell<Option<Target>>,
    timers: Timers,
}

impl KeymapHost<Target> for Host {
    fn is_destroyed(&self) -> bool {
        false
    }
    fn root_target(&self) -> Target {
        self.root.clone()
    }
    fn focused_target(&self) -> Option<Target> {
        self.focused.borrow().clone().filter(|target| !target.0.destroyed.get())
    }
    fn parent_target(&self, target: &Target) -> Option<Target> {
        target.0.parent.clone()
    }
    fn is_target_destroyed(&self, target: &Target) -> bool {
        target.0.destroyed.get()
    }
    fn create_command_event(&self) -> Box<dyn KeymapEvent> {
        Box::new(event("command", "00000", None))
    }
    fn metadata(&self) -> HostMetadata {
        HostMetadata { platform: "linux", primary_modifier: "ctrl", modifiers: [("ctrl", "supported"); 5] }
    }
    fn set_timeout(&self, ms: f64, callback: Box<dyn FnOnce()>) -> u64 {
        let mut timers = self.timers.borrow_mut();
        timers.1 += 1;
        let id = timers.1;
        let at = timers.0 + ms;
        timers.2.push((id, at, Some(callback)));
        id
    }
    fn clear_timeout(&self, id: u64) {
        self.timers.borrow_mut().2.retain(|(timer, _, _)| *timer != id);
    }
}

fn esc(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if (' '..='~').contains(&ch) && ch != '\\' {
            out.push(ch);
        } else {
            let _ = write!(out, "\\u{{{:x}}}", ch as u32);
        }
    }
    out
}

fn dec(text: &str) -> String {
    if text == "%" {
        return String::new();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            out.push(u8::from_str_radix(&text[index + 1..index + 3], 16).unwrap());
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn seq(parts: &[KeySequencePart]) -> String {
    if parts.is_empty() {
        return "-".to_owned();
    }
    parts
        .iter()
        .map(|part| {
            let token = part.token_name.as_ref().map(|token| format!("<{token}>")).unwrap_or_default();
            format!("{}[{}{}]", esc(&part.display), esc(&stringify_key_stroke(&part.stroke)), token)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn attrs(attrs: Option<&Rc<Attrs>>) -> String {
    match attrs {
        None => "-".to_owned(),
        Some(attrs) => attrs.iter().map(|(key, value)| format!("{key}={}", esc(value))).collect::<Vec<_>>().join(","),
    }
}

fn cmd_name(command: Option<&BindingCommand<Target>>) -> String {
    match command {
        None => "-".to_owned(),
        Some(BindingCommand::Run(_)) => "<fn>".to_owned(),
        Some(BindingCommand::Name(name)) => esc(name),
    }
}

fn format_options() -> FormatOptions {
    let pairs = |items: &[(&str, &str)]| items.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect();
    FormatOptions {
        token_display: pairs(&[("leader", "ctrl+x")]),
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

fn visibility(token: &str) -> Visibility {
    match token {
        "registered" => Visibility::Registered,
        "active" => Visibility::Active,
        _ => Visibility::Reachable,
    }
}

fn result(result: &RunCommandResult<Target>) -> String {
    format!(
        "ok={} reason={} command={}",
        result.ok,
        result.reason.map_or("-", RunReason::as_str),
        result.command.as_ref().map_or("-".to_owned(), |command| esc(&command.name))
    )
}

struct LayerSpec {
    slot: bool,
    target: String,
    target_mode: String,
    priority: String,
    enabled: String,
    mode: String,
    bindings: Vec<Vec<String>>,
    commands: Vec<Vec<String>>,
}

struct Program {
    out: Rc<RefCell<Vec<String>>>,
    host: Rc<Host>,
    keymap: Keymap<Target>,
    targets: HashMap<String, Target>,
    vars: Rc<RefCell<HashMap<String, bool>>>,
    specs: HashMap<String, LayerSpec>,
    offs: HashMap<String, Dispose>,
    slots: HashMap<String, LayerSlot<Target>>,
    nested: Rc<Cell<bool>>,
}

impl Program {
    fn new() -> Program {
        let root = Target(Rc::new(TargetData { id: "root".to_owned(), parent: None, destroyed: Cell::new(false) }));
        let host = Rc::new(Host {
            root: root.clone(),
            focused: RefCell::new(None),
            timers: Rc::new(RefCell::new((0.0, 0, Vec::new()))),
        });
        let keymap = Keymap::new(host.clone() as Rc<dyn KeymapHost<Target>>);
        let _ = addons::register_default_keys(&keymap);
        let _ = addons::register_enabled_fields(&keymap);
        let _ = addons::register_metadata_fields(&keymap);
        let _ = addons::register_comma_bindings(&keymap);
        let _ = addons::register_base_layout_fallback(&keymap);
        let _ = addons::register_escape_clears_pending_sequence(&keymap, true, 0);
        let _ = addons::register_backspace_pops_pending_sequence(&keymap, true, 0);
        let _ = keymap.register_layer_fields(vec![(
            "mode",
            Rc::new(|value: &Field, ctx: &mut FieldContext<'_>| {
                let data = match value {
                    Field::Str(text) => Data::Str(text.as_str().into()),
                    Field::Bool(value) => Data::Bool(*value),
                    Field::Num(value) => Data::Num(*value),
                    _ => return Err("unsupported mode".to_owned()),
                };
                ctx.require("opencode.mode", data)
            }) as FieldCompiler,
        )]);
        let out: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = out.clone();
        let _ = keymap
            .on_error(Rc::new(move |code, message| sink.borrow_mut().push(format!("error {code} {}", esc(message)))));
        let sink = out.clone();
        let _ = keymap.on_warning(Rc::new(move |code, message| {
            sink.borrow_mut().push(format!("warning {code} {}", esc(message)))
        }));
        let sink = out.clone();
        let _ = keymap.on_pending_sequence(Rc::new(move |parts| sink.borrow_mut().push(format!("P {}", seq(parts)))));
        keymap.set_data("opencode.mode", Some(Data::from("base")));
        let mut targets = HashMap::new();
        targets.insert("root".to_owned(), root);
        Program {
            out,
            host,
            keymap,
            targets,
            vars: Rc::new(RefCell::new(HashMap::new())),
            specs: HashMap::new(),
            offs: HashMap::new(),
            slots: HashMap::new(),
            nested: Rc::new(Cell::new(false)),
        }
    }

    fn push(&self, line: String) {
        self.out.borrow_mut().push(line);
    }

    fn enabled(&self, token: &str) -> Option<Field> {
        match token {
            "-" => None,
            "t" => Some(Field::Bool(true)),
            "f" => Some(Field::Bool(false)),
            var => {
                let vars = self.vars.clone();
                let var = var.to_owned();
                Some(Field::Fn(Rc::new(move || vars.borrow().get(&var).copied().unwrap_or(false))))
            }
        }
    }

    fn run(&self, spec: &str, name: &str) -> CommandFn<Target> {
        let out = self.out.clone();
        let spec = spec.to_owned();
        let name = name.to_owned();
        let nested = self.nested.clone();
        Rc::new(move |ctx: &CommandContext<'_, Target>| {
            out.borrow_mut().push(format!(
                "run {} input={} target={} focused={} event={} command={}",
                esc(&name),
                esc(&ctx.input),
                ctx.target.as_ref().map_or("-", |t| t.0.id.as_str()),
                ctx.focused.as_ref().map_or("-", |t| t.0.id.as_str()),
                esc(ctx.event.name()),
                ctx.command.as_ref().map_or("-".to_owned(), |command| esc(&command.name))
            ));
            match spec.as_str() {
                "reject" => CommandResult::Rejected,
                "ok" => CommandResult::Result(RunCommandResult::ok()),
                "fail" => CommandResult::Result(RunCommandResult::fail(RunReason::Rejected)),
                "clear" => {
                    ctx.keymap.clear_pending_sequence();
                    CommandResult::Handled
                }
                "mode" => {
                    let base = ctx.keymap.get_data("opencode.mode") == Some(Data::from("base"));
                    ctx.keymap.set_data("opencode.mode", Some(Data::from(if base { "other" } else { "base" })));
                    CommandResult::Handled
                }
                "dispatch" => {
                    if nested.get() {
                        return CommandResult::Handled;
                    }
                    nested.set(true);
                    let r = ctx.keymap.dispatch_command("c0", None, false);
                    out.borrow_mut().push(format!("nested {}", result(&r)));
                    nested.set(false);
                    CommandResult::Handled
                }
                _ => CommandResult::Handled,
            }
        })
    }

    fn build(&self, id: &str) -> Layer<Target> {
        let spec = &self.specs[id];
        let mut layer = Layer::default();
        if spec.target != "-" {
            layer.target = Some(self.targets[&spec.target].clone());
        }
        if spec.target_mode != "-" {
            layer.target_mode =
                Some(if spec.target_mode == "focus" { TargetMode::Focus } else { TargetMode::FocusWithin });
        }
        if spec.priority != "-" {
            layer.priority = Some(spec.priority.parse().unwrap());
        }
        if let Some(enabled) = self.enabled(&spec.enabled) {
            layer.fields.push(("enabled".to_owned(), enabled));
        }
        if spec.mode != "-" {
            layer.fields.push(("mode".to_owned(), Field::Str(dec(&spec.mode))));
        }
        for row in &spec.bindings {
            let cmd = if let Some(name) = row[1].strip_prefix("n:") {
                Some(BindingCommand::Name(dec(name)))
            } else {
                row[1].strip_prefix("f:").map(|run| BindingCommand::Run(self.run(run, "<inline>")))
            };
            let mut binding = Binding::new(dec(&row[0]), cmd);
            if row[2] != "-" {
                binding.event = Some(row[2].clone());
            }
            if row[3] != "-" {
                binding.prevent_default = Some(row[3] == "1");
            }
            if row[4] != "-" {
                binding.fallthrough = Some(row[4] == "1");
            }
            if row[5] != "-" {
                binding.fields.push(("desc".to_owned(), Field::Str(dec(&row[5]))));
            }
            if row[6] != "-" {
                binding.fields.push(("group".to_owned(), Field::Str(dec(&row[6]))));
            }
            if row[7] != "-" {
                binding.fields.push((dec(&row[7]), Field::Bool(true)));
            }
            layer.bindings.push(binding);
        }
        for row in &spec.commands {
            let name = dec(&row[0]);
            let mut command = Command { name: name.clone(), run: Some(self.run(&row[1], &name)), fields: Vec::new() };
            if let Some(enabled) = self.enabled(&row[2]) {
                command.fields.push(("enabled".to_owned(), enabled));
            }
            if row[3] != "-" {
                command.fields.push(("title".to_owned(), Field::Str(dec(&row[3]))));
            }
            if row[4] != "-" {
                command.fields.push(("desc".to_owned(), Field::Str(dec(&row[4]))));
            }
            if row[5] != "-" {
                command.fields.push(("category".to_owned(), Field::Str(dec(&row[5]))));
            }
            layer.commands.push(command);
        }
        layer
    }

    fn active_keys(&self) -> String {
        self.keymap
            .get_active_keys(true, true)
            .iter()
            .map(|key| {
                let token = key.token_name.as_ref().map(|token| format!("<{token}>")).unwrap_or_default();
                format!(
                    "{}[{}]{} c={} cmd={} b={} ca={} n={}",
                    esc(&key.display),
                    esc(&stringify_key_stroke(&key.stroke)),
                    token,
                    key.continues as u8,
                    cmd_name(key.command.as_ref()),
                    attrs(key.binding_attrs.as_ref()),
                    attrs(key.command_attrs.as_ref()),
                    key.bindings.as_ref().map_or(0, Vec::len)
                )
            })
            .collect::<Vec<_>>()
            .join(" ; ")
    }

    fn advance(&self, ms: f64) {
        let target = self.host.timers.borrow().0 + ms;
        loop {
            let due = {
                let mut timers = self.host.timers.borrow_mut();
                let best = timers
                    .2
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, at, _))| *at <= target)
                    .min_by(|a, b| a.1.1.partial_cmp(&b.1.1).unwrap().then(a.1.0.cmp(&b.1.0)))
                    .map(|(index, _)| index);
                best.map(|index| {
                    let (_, at, callback) = timers.2.remove(index);
                    timers.0 = at;
                    callback
                })
            };
            match due {
                Some(Some(callback)) => callback(),
                Some(None) => {}
                None => break,
            }
        }
        self.host.timers.borrow_mut().0 = target;
    }

    fn exec(&mut self, op: &str) {
        let tokens: Vec<&str> = op.split(' ').collect();
        let (name, args) = (tokens[0], &tokens[1..]);
        match name {
            "target" => {
                let parent = self.targets.get(args[1]).cloned();
                let target =
                    Target(Rc::new(TargetData { id: args[0].to_owned(), parent, destroyed: Cell::new(false) }));
                self.targets.insert(args[0].to_owned(), target);
            }
            "var" => {
                self.vars.borrow_mut().insert(args[0].to_owned(), args[1] == "1");
            }
            "leader" => {
                let timeout = args[1].parse().unwrap();
                if let Err(error) = addons::register_timed_leader(
                    &self.keymap,
                    &KeyLike::Str(dec(args[0])),
                    Some("leader"),
                    Some(timeout),
                ) {
                    self.push(format!("leader-error {}", esc(&error)));
                }
            }
            "layer" => {
                self.specs.insert(
                    args[0].to_owned(),
                    LayerSpec {
                        slot: args[1] == "slot",
                        target: args[2].to_owned(),
                        target_mode: args[3].to_owned(),
                        priority: args[4].to_owned(),
                        enabled: args[5].to_owned(),
                        mode: args[6].to_owned(),
                        bindings: Vec::new(),
                        commands: Vec::new(),
                    },
                );
            }
            "bind" => {
                self.specs.get_mut(args[0]).unwrap().bindings.push(args[1..].iter().map(|s| (*s).to_owned()).collect())
            }
            "cmd" => {
                self.specs.get_mut(args[0]).unwrap().commands.push(args[1..].iter().map(|s| (*s).to_owned()).collect())
            }
            "commit" => {
                let layer = self.build(args[0]);
                if self.specs[args[0]].slot {
                    if !self.slots.contains_key(args[0]) {
                        self.slots.insert(args[0].to_owned(), self.keymap.create_layer_slot());
                    }
                    self.keymap.replace_layer(&self.slots[args[0]], Some(layer));
                } else {
                    let off = self.keymap.register_layer(layer);
                    if let Some(previous) = self.offs.insert(args[0].to_owned(), off) {
                        drop(previous);
                    }
                }
                self.push(format!("committed {}", args[0]));
            }
            "unreg" => {
                if self.specs.get(args[0]).is_some_and(|spec| spec.slot) {
                    if let Some(slot) = self.slots.get(args[0]) {
                        self.keymap.replace_layer(slot, None);
                    }
                } else if let Some(off) = self.offs.remove(args[0]) {
                    off();
                }
                self.push(format!("unregistered {}", args[0]));
            }
            "press" | "release" => {
                let base = (args[2] != "-").then(|| args[2].parse().unwrap());
                let e = event(&dec(args[0]), args[1], base);
                self.keymap.handle_key_event(&e, name == "release");
                let pending = seq(&self.keymap.get_pending_sequence());
                self.push(format!(
                    "{name} prevented={} stopped={} pending={pending}",
                    e.prevented.get() as u8,
                    e.stopped.get() as u8
                ));
            }
            "focus" => {
                *self.host.focused.borrow_mut() =
                    if args[0] == "-" { None } else { Some(self.targets[args[0]].clone()) };
                self.keymap.handle_focus_change();
                self.push(format!("focused {}", args[0]));
            }
            "destroy" => {
                let target = self.targets[args[0]].clone();
                target.0.destroyed.set(true);
                self.keymap.handle_target_destroyed(&target);
                self.push(format!("destroyed {}", args[0]));
            }
            "data" => {
                let value = (args[1] != "-").then(|| Data::Str(dec(args[1]).as_str().into()));
                self.keymap.set_data(args[0], value);
                self.push(format!("data {}", args[0]));
            }
            "adv" => {
                self.advance(args[0].parse().unwrap());
                let now = self.host.timers.borrow().0;
                let pending = seq(&self.keymap.get_pending_sequence());
                self.push(format!("now {now} pending={pending}"));
            }
            "dispatch" => {
                let r = self.keymap.dispatch_command(&dec(args[0]), None, true);
                self.push(format!("dispatched {}", result(&r)));
            }
            "runcmd" => {
                let r = self.keymap.run_command(&dec(args[0]), None, true);
                self.push(format!("ran {}", result(&r)));
            }
            "pending" => {
                let pending = seq(&self.keymap.get_pending_sequence());
                let has = self.keymap.has_pending_sequence() as u8;
                self.push(format!("pending {pending} has={has}"));
            }
            "pop" => {
                let popped = self.keymap.pop_pending_sequence() as u8;
                let pending = seq(&self.keymap.get_pending_sequence());
                self.push(format!("pop {popped} {pending}"));
            }
            "clear" => {
                self.keymap.clear_pending_sequence();
                self.push("cleared".to_owned());
            }
            "active" => {
                let keys = self.active_keys();
                self.push(format!("active {keys}"));
            }
            "commands" => {
                let commands = self.keymap.get_commands(visibility(args[0]), None);
                let line = commands
                    .iter()
                    .map(|command| {
                        format!(
                            "{}:{}",
                            esc(&command.name),
                            command.field("title").and_then(Field::as_str).unwrap_or("-")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                self.push(format!("commands {line}"));
            }
            "entries" => {
                let entries = self.keymap.get_command_entries(visibility(args[0]), None);
                let line = entries
                    .iter()
                    .map(|entry| {
                        let bindings = entry
                            .bindings
                            .iter()
                            .map(|b| format!("{}/{}", seq(&b.sequence), attrs(b.command_attrs.as_ref())))
                            .collect::<Vec<_>>()
                            .join("|");
                        format!("{}={bindings}", esc(&entry.command.name))
                    })
                    .collect::<Vec<_>>()
                    .join(" ; ");
                self.push(format!("entries {line}"));
            }
            "bindings" => {
                let names: Vec<String> = args[1..].iter().map(|name| dec(name)).collect();
                let rows = self.keymap.get_command_bindings(visibility(args[0]), &names, None);
                let options = format_options();
                let line = rows
                    .iter()
                    .map(|(command, bindings)| {
                        let listed = bindings
                            .iter()
                            .map(|b| {
                                format!(
                                    "{}/{}/{}{}/{}/{}",
                                    seq(&b.sequence),
                                    if b.release { "release" } else { "press" },
                                    b.prevent_default as u8,
                                    b.fallthrough as u8,
                                    attrs(b.attrs.as_ref()),
                                    attrs(b.command_attrs.as_ref())
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("|");
                        let formatted = format_command_bindings(
                            bindings.iter().map(|b| b.sequence.as_slice()),
                            &options,
                            None,
                            true,
                        );
                        let first = format_key_sequence(bindings.first().map(|b| b.sequence.as_slice()), &options);
                        format!(
                            "{}={listed} fmt={} first={}",
                            esc(command),
                            esc(formatted.as_deref().unwrap_or("-")),
                            esc(&first)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ; ");
                self.push(format!("bindings {line}"));
            }
            "parse" => {
                let key = KeyLike::Str(dec(args[0]));
                match self.keymap.parse_key_sequence(&key) {
                    Ok(parts) => {
                        let again = self.keymap.parse_key_sequence(&key).unwrap();
                        let formatted = format_key_sequence(Some(&again), &format_options());
                        self.push(format!("parsed {} fmt={}", seq(&parts), esc(&formatted)));
                    }
                    Err(error) => self.push(format!("parse-error {}", esc(&error))),
                }
            }
            "matcher" => match self.keymap.create_key_matcher(&KeyLike::Str(dec(args[0]))) {
                Ok(matcher) => {
                    let e = event(&dec(args[1]), args[2], None);
                    match matcher.matches_event(&e) {
                        Ok(matched) => self.push(format!("matcher {}", matched as u8)),
                        Err(error) => self.push(format!("matcher-error {}", esc(&error))),
                    }
                }
                Err(error) => self.push(format!("matcher-error {}", esc(&error))),
            },
            _ => panic!("op {op}"),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let programs = std::fs::read_to_string(&args[1]).expect("programs");
    let mut results = String::new();
    let mut program: Option<Program> = None;
    for line in programs.lines() {
        if line == "end" {
            if let Some(done) = program.take() {
                for answer in done.out.borrow().iter() {
                    results.push_str(answer);
                    results.push('\n');
                }
            }
            results.push_str("end\n");
            continue;
        }
        program.get_or_insert_with(Program::new).exec(line);
    }
    std::fs::write(&args[2], results).expect("results");
}
