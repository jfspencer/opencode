use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_core::elements::input;
use opencode_tui_core::input::{KeyEvent, ParsedKey};
use opencode_tui_core::renderables::textarea::TextareaKind;
use opencode_tui_core::tree::{self, Kind};

fn key(name: &str, sequence: &str) -> KeyEvent {
    KeyEvent::new(ParsedKey { name: name.into(), sequence: sequence.into(), ..ParsedKey::default() })
}

#[test]
fn typing_reaches_on_input_and_enter_submits() {
    tree::create_root_for_tests(40, 10);
    let root = tree::root().unwrap();
    let el = input();
    root.add(el, None);
    let seen = Rc::new(RefCell::new(Vec::new()));
    {
        let seen = seen.clone();
        el.set_on_input(Some(Rc::new(move |value| seen.borrow_mut().push(value))));
    }
    for (name, sequence) in [("a", "a"), ("b", "b")] {
        let mut event = key(name, sequence);
        el.with_kind::<TextareaKind, _>(|k| k.handle_key_press(el, &mut event));
    }
    assert_eq!(el.input_value(), "ab");
    assert_eq!(*seen.borrow(), ["a", "ab"]);
}
