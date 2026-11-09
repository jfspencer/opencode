use std::cell::RefCell;
use std::rc::Rc;

use opencode_tui_core::elements::{box_, scrollbox};
use opencode_tui_core::layout::Value;
use opencode_tui_core::renderables::scrollbox::{ScrollBarOptions, ScrollUnit, StickyStart};
use opencode_tui_core::tree::{self, Edge};

fn rows(parent: tree::Element, count: usize) -> Vec<tree::Element> {
    (0..count)
        .map(|_| {
            let row = box_();
            row.set_height(Some(Value::Point(1.0)));
            parent.add(row, None);
            row
        })
        .collect()
}

#[test]
fn scrolls_its_children_within_the_viewport() {
    tree::create_root_for_tests(40, 20);
    let root = tree::root().unwrap();
    let scroll = scrollbox();
    root.add(scroll, None);
    scroll.set_max_height(Some(Value::Point(4.0)));
    scroll.set_padding(Edge::Left, Some(Value::Point(1.0)));
    scroll.set_scrollbar_options(None, &ScrollBarOptions { visible: Some(Some(false)), ..Default::default() });
    let children = rows(scroll, 10);
    let changes = Rc::new(RefCell::new(Vec::new()));
    {
        let changes = changes.clone();
        scroll.on_scroll_change(true, Rc::new(move |position| changes.borrow_mut().push(position)));
    }
    tree::prepare_frame_for_tests(40, 20);
    assert_eq!(scroll.height(), 4.0);
    assert_eq!(scroll.scroll_height(), 10.0);
    assert_eq!(scroll.scroll_max(1), 6.0);
    assert_eq!(scroll.scroll_viewport().height, 4.0);
    assert_eq!(children[2].y(), 2);
    // Padding goes to the content: children start one cell in.
    assert_eq!(children[0].x(), 1);
    scroll.scroll_by(3.0, ScrollUnit::Absolute);
    assert_eq!(scroll.scroll_top(), 3.0);
    // Positions compose the offset at once, before the next layout.
    assert_eq!(children[2].y(), -1);
    assert_eq!(*changes.borrow(), [3.0]);
    scroll.scroll_by(10.0, ScrollUnit::Absolute);
    assert_eq!(scroll.scroll_top(), 6.0);
    tree::prepare_frame_for_tests(40, 20);
    assert_eq!(children[6].y(), 0);
    scroll.scroll_to(0.0);
    assert_eq!(scroll.scroll_top(), 0.0);
    assert!(!scroll.scrollbar_visible(true));
}

#[test]
fn sticky_bottom_follows_growing_content() {
    tree::create_root_for_tests(40, 20);
    let root = tree::root().unwrap();
    let scroll = scrollbox();
    root.add(scroll, None);
    scroll.set_height(Some(Value::Point(3.0)));
    scroll.set_sticky_scroll(true);
    scroll.set_sticky_start(Some(StickyStart::Bottom));
    rows(scroll, 5);
    tree::prepare_frame_for_tests(40, 20);
    // The bar shows (auto) and takes a gutter; the view follows the bottom.
    assert!(scroll.scrollbar_visible(true));
    assert_eq!(scroll.scroll_top(), 2.0);
    rows(scroll, 2);
    tree::prepare_frame_for_tests(40, 20);
    assert_eq!(scroll.scroll_top(), 4.0);
    scroll.set_scroll_top(1.0);
    rows(scroll, 1);
    tree::prepare_frame_for_tests(40, 20);
    assert_eq!(scroll.scroll_top(), 1.0);
}
