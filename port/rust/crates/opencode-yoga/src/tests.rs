use super::*;
use std::cell::RefCell;
use std::rc::Rc;

fn layout(node: &Node) -> (f32, f32, f32, f32) {
    let l = node.get_computed_layout().unwrap();
    (l.left, l.top, l.width, l.height)
}

#[test]
fn row_with_grow_and_percent() {
    let config = Config::create().unwrap();
    let root = Node::create(Some(&config)).unwrap();
    root.set_width(100).unwrap();
    root.set_height(50).unwrap();
    root.set_flex_direction(FlexDirection::Row).unwrap();
    let a = Node::create(Some(&config)).unwrap();
    a.set_flex_grow(Some(1.0)).unwrap();
    root.insert_child(&a, 0).unwrap();
    let b = Node::create(Some(&config)).unwrap();
    b.set_width("33.3%").unwrap();
    root.insert_child(&b, 1).unwrap();
    root.calculate_layout(None, None, Direction::LTR).unwrap();
    assert_eq!(layout(&root), (0.0, 0.0, 100.0, 50.0));
    assert_eq!(layout(&a), (0.0, 0.0, 67.0, 50.0));
    assert_eq!(layout(&b), (67.0, 0.0, 33.0, 50.0));
    assert_eq!(b.get_width().unwrap(), Value { unit: Unit::Percent, value: 33.3 });
    assert_eq!(root.get_child(1).unwrap(), Some(b.clone()));
    assert_eq!(b.get_parent(), Some(root.clone()));
}

#[test]
fn measure_and_dirtied() {
    let root = Node::create(None).unwrap();
    root.set_width(20).unwrap();
    let text = Node::create(None).unwrap();
    root.insert_child(&text, 0).unwrap();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let seen = calls.clone();
    text.set_measure_func(Some(Box::new(move |width, mode, _, _| {
        seen.borrow_mut().push((width, mode));
        Size { width: 7.5, height: 2.0 }
    })))
    .unwrap();
    assert_eq!(text.get_node_type(), NodeType::Text);
    let dirtied = Rc::new(RefCell::new(0));
    let count = dirtied.clone();
    root.set_dirtied_func(Some(Box::new(move |node| {
        assert!(node.get_child_count() == 1);
        *count.borrow_mut() += 1;
    })))
    .unwrap();
    root.calculate_layout(None, None, Direction::LTR).unwrap();
    assert_eq!(calls.borrow().as_slice(), &[(20.0, MeasureMode::Exactly)]);
    // Text nodes round their size up.
    assert_eq!(layout(&text), (0.0, 0.0, 20.0, 2.0));
    text.mark_dirty().unwrap();
    assert_eq!(*dirtied.borrow(), 1);
    assert!(root.is_dirty().unwrap());
}

#[test]
fn a_mutation_inside_a_callback_fails() {
    let root = Node::create(None).unwrap();
    let leaf = Node::create(None).unwrap();
    root.insert_child(&leaf, 0).unwrap();
    let result = Rc::new(RefCell::new(None));
    let out = result.clone();
    let other = Node::create(None).unwrap();
    leaf.set_measure_func(Some(Box::new(move |_, _, _, _| {
        *out.borrow_mut() = Some(other.set_width(3));
        Size { width: 1.0, height: 1.0 }
    })))
    .unwrap();
    root.calculate_layout(Some(10.0), Some(10.0), Direction::LTR).unwrap();
    assert_eq!(*result.borrow(), Some(Err(Error::InCallback)));
    assert_eq!(Error::InCallback.to_string(), "Cannot mutate Yoga during a callback");
}

#[test]
fn a_panicking_callback_resumes_after_the_call() {
    let root = Node::create(None).unwrap();
    let leaf = Node::create(None).unwrap();
    root.insert_child(&leaf, 0).unwrap();
    leaf.set_measure_func(Some(Box::new(|_, _, _, _| panic!("measure failed")))).unwrap();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| root.calculate_layout(None, None, Direction::LTR)));
    assert!(outcome.is_err());
    // The layout itself completed with the unspecified size.
    assert!(!root.is_poisoned());
    leaf.unset_measure_func().unwrap();
    root.calculate_layout(Some(5.0), None, Direction::LTR).unwrap();
    assert_eq!(layout(&leaf), (0.0, 0.0, 5.0, 0.0));
}

#[test]
fn parents_keep_children_alive() {
    let root = Node::create(None).unwrap();
    let weak;
    {
        let child = Node::create(None).unwrap();
        child.set_height(4).unwrap();
        root.insert_child(&child, 0).unwrap();
        weak = Rc::downgrade(&child.0);
    }
    assert!(weak.upgrade().is_some());
    root.calculate_layout(Some(9.0), None, Direction::LTR).unwrap();
    assert_eq!(layout(&root.get_child(0).unwrap().unwrap()), (0.0, 0.0, 9.0, 4.0));
    root.remove_all_children().unwrap();
    assert!(weak.upgrade().is_none());
}

#[test]
fn dropping_a_root_frees_its_tree_but_not_held_children() {
    let held = Node::create(None).unwrap();
    let weak;
    {
        let root = Node::create(None).unwrap();
        let middle = Node::create(None).unwrap();
        root.insert_child(&middle, 0).unwrap();
        middle.insert_child(&held, 0).unwrap();
        weak = Rc::downgrade(&middle.0);
    }
    assert!(weak.upgrade().is_none());
    assert_eq!(held.get_parent(), None);
    assert!(!held.is_freed());
}

#[test]
fn a_root_dropped_in_its_dirtied_callback_is_freed_after_the_call() {
    let slot: Rc<RefCell<Option<Node>>> = Rc::new(RefCell::new(None));
    let root = Node::create(None).unwrap();
    let child = Node::create(None).unwrap();
    root.insert_child(&child, 0).unwrap();
    let taken = slot.clone();
    root.set_dirtied_func(Some(Box::new(move |_| drop(taken.borrow_mut().take())))).unwrap();
    root.calculate_layout(None, None, Direction::LTR).unwrap();
    let weak = Rc::downgrade(&root.0);
    *slot.borrow_mut() = Some(root);
    child.set_width(3).unwrap();
    assert!(weak.upgrade().is_none());
    assert_eq!(child.get_parent(), None);
}

#[test]
fn freed_nodes_answer_defaults() {
    let node = Node::create(None).unwrap();
    node.set_width(5).unwrap();
    node.free().unwrap();
    assert!(node.is_freed());
    assert_eq!(node.get_computed_layout().unwrap(), Layout::default());
    assert!(node.get_width().unwrap().value.is_nan());
    assert_eq!(node.get_flex_direction().unwrap(), FlexDirection::Column);
    assert!(node.is_dirty().unwrap());
    node.set_height(3).unwrap();
    let parent = Node::create(None).unwrap();
    assert_eq!(parent.insert_child(&node, 0), Err(Error::NodeFreed));
    assert_eq!(
        node.style_set_enum(0, 0).unwrap_err().to_string(),
        "yogaNodeStyleSetEnumChecked failed: InvalidArgument (status 1)"
    );
}

#[test]
fn placement_errors() {
    let a = Node::create(None).unwrap();
    let b = Node::create(None).unwrap();
    a.insert_child(&b, 0).unwrap();
    let status = |result: Result<()>| match result {
        Err(Error::Yoga { status, .. }) => status,
        other => panic!("{other:?}"),
    };
    assert_eq!(status(b.insert_child(&a, 0)), Status::InvalidArgument);
    assert_eq!(status(a.insert_child(&b, 0)), Status::InvalidArgument);
    let c = Node::create(None).unwrap();
    assert_eq!(status(a.insert_child(&c, 2)), Status::InvalidArgument);
    // A chain at the depth limit.
    let mut chain = vec![Node::create(None).unwrap()];
    for _ in 1..DEPTH_MAX {
        let next = Node::create(None).unwrap();
        chain.last().unwrap().insert_child(&next, 0).unwrap();
        chain.push(next);
    }
    let extra = Node::create(None).unwrap();
    assert_eq!(status(chain.last().unwrap().insert_child(&extra, 0)), Status::DepthLimit);
    // Moving within a parent and across parents.
    a.insert_child(&c, 1).unwrap();
    a.move_child(&c, 0).unwrap();
    assert_eq!(a.get_child(0).unwrap(), Some(c.clone()));
    let d = Node::create(None).unwrap();
    d.move_child(&b, 0).unwrap();
    assert_eq!(b.get_parent(), Some(d.clone()));
    assert_eq!(a.get_child_count(), 1);
}

#[test]
fn value_parsing_errors() {
    assert_eq!(ValueInput::parse("12").unwrap_err().to_string(), "Invalid Yoga value: 12");
    assert_eq!(ValueInput::parse("x%").unwrap_err().to_string(), "Invalid Yoga percentage value: x%");
    let node = Node::create(None).unwrap();
    assert_eq!(
        node.set_width("Infinity%").unwrap_err().to_string(),
        "yogaNodeStyleSetValueChecked failed: InvalidArgument (status 1)"
    );
}

#[test]
fn logger_receives_messages() {
    let config = Config::create().unwrap();
    let messages = Rc::new(RefCell::new(Vec::new()));
    let sink = messages.clone();
    config.set_logger(Some(Box::new(move |level, text| sink.borrow_mut().push((level, text.to_owned()))))).unwrap();
    drop(config);
}
