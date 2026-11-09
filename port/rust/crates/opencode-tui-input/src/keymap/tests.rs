use super::*;
use std::any::Any;
use std::cell::{Cell, RefCell};

#[derive(Clone, PartialEq)]
struct Target(Rc<(&'static str, Option<Target>)>);

struct Host {
    root: Target,
    focused: RefCell<Option<Target>>,
    timers: RefCell<Vec<(u64, Box<dyn FnOnce()>)>>,
}

impl KeymapHost<Target> for Host {
    fn is_destroyed(&self) -> bool {
        false
    }
    fn root_target(&self) -> Target {
        self.root.clone()
    }
    fn focused_target(&self) -> Option<Target> {
        self.focused.borrow().clone()
    }
    fn parent_target(&self, target: &Target) -> Option<Target> {
        target.0.1.clone()
    }
    fn is_target_destroyed(&self, _: &Target) -> bool {
        false
    }
    fn create_command_event(&self) -> Box<dyn KeymapEvent> {
        Box::new(Press::new("command", false))
    }
    fn metadata(&self) -> HostMetadata {
        HostMetadata { platform: "linux", primary_modifier: "ctrl", modifiers: [("ctrl", "supported"); 5] }
    }
    fn set_timeout(&self, _: f64, callback: Box<dyn FnOnce()>) -> u64 {
        let mut timers = self.timers.borrow_mut();
        let id = timers.len() as u64 + 1;
        timers.push((id, callback));
        id
    }
    fn clear_timeout(&self, id: u64) {
        self.timers.borrow_mut().retain(|(timer, _)| *timer != id);
    }
}

struct Press {
    name: String,
    ctrl: bool,
    prevented: Cell<bool>,
    stopped: Cell<bool>,
}

impl Press {
    fn new(name: &str, ctrl: bool) -> Press {
        Press { name: name.to_owned(), ctrl, prevented: Cell::new(false), stopped: Cell::new(false) }
    }
}

impl KeymapEvent for Press {
    fn name(&self) -> &str {
        &self.name
    }
    fn ctrl(&self) -> bool {
        self.ctrl
    }
    fn shift(&self) -> bool {
        false
    }
    fn meta(&self) -> bool {
        false
    }
    fn super_key(&self) -> bool {
        false
    }
    fn hyper(&self) -> bool {
        false
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

fn setup() -> (Rc<Host>, Keymap<Target>, Rc<RefCell<Vec<String>>>) {
    let host = Rc::new(Host {
        root: Target(Rc::new(("root", None))),
        focused: RefCell::new(None),
        timers: RefCell::new(Vec::new()),
    });
    let keymap = Keymap::create_default(host.clone());
    let _ = addons::register_comma_bindings(&keymap);
    (host, keymap, Rc::new(RefCell::new(Vec::new())))
}

fn command(log: &Rc<RefCell<Vec<String>>>, name: &str) -> Command<Target> {
    let log = log.clone();
    let label = name.to_owned();
    Command {
        name: name.to_owned(),
        run: Some(Rc::new(move |ctx| {
            log.borrow_mut().push(format!("{label}:{}", ctx.input));
            CommandResult::Handled
        })),
        fields: Vec::new(),
    }
}

fn press(keymap: &Keymap<Target>, name: &str, ctrl: bool) -> bool {
    let event = Press::new(name, ctrl);
    keymap.handle_key_event(&event, false);
    event.prevented.get()
}

#[test]
fn sequences_leader_and_timeout() {
    let (host, keymap, log) = setup();
    let _leader =
        addons::register_timed_leader(&keymap, &KeyLike::from("ctrl+x"), Some("leader"), Some(2000.0)).unwrap();
    let _ = keymap.register_layer(Layer {
        commands: vec![command(&log, "quit"), command(&log, "top")],
        bindings: vec![
            Binding::new("<leader>q", Some(BindingCommand::Name("quit".into()))),
            Binding::new("gg", Some(BindingCommand::Name("top".into()))),
        ],
        ..Layer::default()
    });
    assert!(press(&keymap, "x", true));
    assert_eq!(keymap.get_pending_sequence()[0].display, "<leader>");
    assert!(press(&keymap, "q", false));
    assert_eq!(*log.borrow(), ["quit:quit"]);
    assert!(press(&keymap, "g", false));
    assert!(press(&keymap, "g", false));
    assert_eq!(log.borrow().last().unwrap(), "top:top");
    // The leader's timer clears a pending leader sequence.
    press(&keymap, "x", true);
    let (_, fire) = host.timers.borrow_mut().pop().unwrap();
    fire();
    assert!(keymap.get_pending_sequence().is_empty());
}

#[test]
fn priority_then_newest_layer_wins() {
    let (_, keymap, log) = setup();
    for (name, priority) in [("low", 0), ("high", 1), ("newer", 0)] {
        let _ = keymap.register_layer(Layer {
            priority: Some(priority),
            commands: vec![command(&log, name)],
            bindings: vec![Binding::new("j,k", Some(BindingCommand::Name(name.into())))],
            ..Layer::default()
        });
    }
    press(&keymap, "k", false);
    assert_eq!(*log.borrow(), ["high:high"]);
    let active = keymap.get_active_keys(false, false);
    assert_eq!(active.iter().map(|key| key.display.as_str()).collect::<Vec<_>>(), ["j", "k"]);
}

#[test]
fn focus_targets_and_kept_slots() {
    let (host, keymap, log) = setup();
    let child = Target(Rc::new(("child", Some(host.root.clone()))));
    let slot = keymap.create_layer_slot();
    let layer = |log: &Rc<RefCell<Vec<String>>>| Layer {
        target: Some(child.clone()),
        commands: vec![command(log, "local")],
        bindings: vec![Binding::new("ab", Some(BindingCommand::Name("local".into())))],
        ..Layer::default()
    };
    keymap.replace_layer(&slot, Some(layer(&log)));
    assert!(!press(&keymap, "a", false));
    *host.focused.borrow_mut() = Some(child.clone());
    keymap.handle_focus_change();
    assert!(press(&keymap, "a", false));
    // The same layer again keeps its place and the pending sequence.
    keymap.replace_layer(&slot, Some(layer(&log)));
    assert_eq!(keymap.get_pending_sequence().len(), 1);
    press(&keymap, "b", false);
    assert_eq!(*log.borrow(), ["local:local"]);
    let result = keymap.dispatch_command("local", None, true);
    assert!(result.ok);
    assert_eq!(keymap.dispatch_command("missing", None, false).reason, Some(RunReason::NotFound));
}
