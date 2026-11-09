//! The textarea's edit actions (EditBufferRenderable's methods) and its suspended key handling.
use opencode_tui_core::elements::textarea;
use opencode_tui_core::input::{KeyEvent, ParsedKey};
use opencode_tui_core::renderables::textarea::{EditorTraits, TextareaAction as A};
use opencode_tui_core::tree;

fn key(name: &str, sequence: &str) -> KeyEvent {
    KeyEvent::new(ParsedKey { name: name.into(), sequence: sequence.into(), ..ParsedKey::default() })
}

fn setup(text: &str) -> opencode_tui_core::tree::Element {
    tree::create_root_for_tests(40, 6);
    let input = textarea();
    tree::root().unwrap().add(input, None);
    input.set_textarea_text(text);
    input.focus();
    tree::prepare_frame_for_tests(40, 6);
    input
}

#[test]
fn keyboard_selection_extends_and_deletes() {
    let input = setup("hello world");
    assert!(input.textarea_action(A::SelectLeft));
    assert!(input.textarea_action(A::SelectLeft));
    assert_eq!(input.textarea_selected_text(), "ld");
    assert!(input.textarea_action(A::SelectWordBackward));
    assert_eq!(input.textarea_selected_text(), "world");
    input.textarea_action(A::Backspace);
    assert_eq!(input.textarea_text(), "hello ");
}

#[test]
fn horizontal_moves_collapse_a_selection_to_its_edge() {
    let input = setup("abcdef");
    input.textarea_action(A::SelectLeft);
    input.textarea_action(A::SelectLeft);
    input.textarea_action(A::MoveLeft);
    assert_eq!(input.textarea_selected_text(), "");
    input.textarea_insert_text("X");
    assert_eq!(input.textarea_text(), "abcdXef");
}

#[test]
fn select_all_then_typing_replaces() {
    let input = setup("one\ntwo");
    input.textarea_action(A::SelectAll);
    assert_eq!(input.textarea_selected_text(), "one\ntwo");
    input.textarea_insert_text("z");
    assert_eq!(input.textarea_text(), "z");
}

#[test]
fn line_home_at_column_zero_goes_to_the_previous_line_end() {
    let input = setup("ab\ncd");
    input.textarea_action(A::LineHome);
    input.textarea_action(A::LineHome);
    input.textarea_insert_text("!");
    assert_eq!(input.textarea_text(), "ab!\ncd");
}

#[test]
fn delete_to_line_start_and_end() {
    let input = setup("abc def");
    input.textarea_action(A::WordBackward);
    input.textarea_action(A::DeleteToLineStart);
    assert_eq!(input.textarea_text(), "def");
    input.textarea_action(A::DeleteToLineEnd);
    assert_eq!(input.textarea_text(), "");
}

#[test]
fn suspended_textareas_only_insert_printable_keys() {
    let input = setup("ab");
    let mut left = key("left", "\x1b[D");
    assert!(tree::handle_key_press_for_tests(input, &mut left));
    input.textarea_insert_text("X");
    assert_eq!(input.textarea_text(), "aXb");
    input.set_textarea_traits(EditorTraits { suspend: true, ..EditorTraits::default() });
    let mut left = key("left", "\x1b[D");
    assert!(!tree::handle_key_press_for_tests(input, &mut left));
    let mut y = key("y", "y");
    assert!(tree::handle_key_press_for_tests(input, &mut y));
    assert_eq!(input.textarea_text(), "aXyb");
}
