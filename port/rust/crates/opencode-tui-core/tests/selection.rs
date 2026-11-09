//! The renderer's selection across renderables (`startSelection`, `updateSelection`, `getSelectedText`).
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::selection::{self, SelectionBehavior};
use opencode_tui_core::tree;

#[test]
fn a_selection_spans_texts_in_reading_order() {
    tree::create_root_for_tests(20, 4);
    let column = box_();
    tree::root().unwrap().add(column, None);
    let first = text();
    first.set_content("hello");
    let second = text();
    second.set_content("world");
    column.add(first, None);
    column.add(second, None);
    tree::prepare_frame_for_tests(20, 4);
    assert!(first.should_start_selection(1, 0));
    selection::start_selection(first, 1, 0, SelectionBehavior::Cell);
    selection::update_selection(Some(second), 2, 1, true);
    let current = selection::get_selection().expect("a selection");
    assert_eq!(current.selected, vec![first, second]);
    let selected = current.selected_text();
    assert!(selected.starts_with("ello\nwo"), "{selected:?}");
    assert!(first.has_selection() && second.has_selection());
    selection::clear_selection();
    assert!(selection::get_selection().is_none());
    assert!(!first.has_selection() && !second.has_selection());
}
