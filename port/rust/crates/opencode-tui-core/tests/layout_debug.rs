use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::BorderProp;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{self, Align, Justify};

#[test]
fn bordered_box_children_start_inside_the_border() {
    tree::create_root_for_tests(50, 14);
    let root = tree::root().unwrap();
    let (count, set_count) = create_signal(0);
    let mut panel_el = None;
    let mut label_el = None;
    let _d = render(
        || {
            let outer = box_();
            outer.set_flex_grow(Some(1.0));
            outer.set_align_items(Align::Center);
            outer.set_justify_content(Justify::Center);
            let panel = box_();
            insert_node(outer, panel, None);
            panel.set_border(BorderProp::All(true));
            panel.set_width(Some(Value::Point(30.0)));
            let label = text();
            insert_node(panel, label, None);
            insert(label, View::dynamic(move || View::from(format!("count: {}", count.get()))), At::Only);
            panel_el = Some(panel);
            label_el = Some(label);
            View::Node(outer)
        },
        root,
    );
    for round in 0..3 {
        tree::prepare_frame_for_tests(50, 14);
        let p = panel_el.unwrap().geometry();
        let l = label_el.unwrap().geometry();
        eprintln!("round {round}: panel {p:?} label {l:?}");
        // A measured node at a half position floors its top (Yoga textRounding).
        assert_eq!(l.screen_y, (p.screen_y + 0.5).floor(), "round {round}");
        set_count.set(round + 1);
    }
}
