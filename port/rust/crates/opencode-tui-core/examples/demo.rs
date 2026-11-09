//! A small screen on the whole pipeline (terminal setup, reactive tree, layout, paint, diffed output, keys):
//! a bordered box with a title, a counter that `+` increments, a list toggled by `t`; `q` quits.
use opencode_tui_core::color::parse_color;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::BorderProp;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{self, Align, Justify};

fn app() -> View {
    let (count, set_count) = create_signal(0);
    let (open, set_open) = create_signal(true);
    renderer::on_key(move |key| match key.name() {
        "q" => renderer::destroy(),
        "+" => set_count.set_with(|c| c + 1),
        "t" => set_open.set_with(|o| !o),
        _ => {}
    });
    let root = box_();
    root.set_flex_grow(Some(1.0));
    root.set_align_items(Align::Center);
    root.set_justify_content(Justify::Center);
    let panel = box_();
    insert_node(root, panel, None);
    panel.set_border(BorderProp::All(true));
    panel.set_border_color(Some(parse_color("#5555ff")));
    panel.set_title(Some(" demo ".into()));
    panel.set_width(Some(Value::Point(30.0)));
    panel.set_padding(tree::Edge::Horizontal, Some(Value::Point(1.0)));
    let label = text();
    insert_node(panel, label, None);
    insert(label, View::dynamic(move || View::from(format!("count: {}", count.get()))), At::Only);
    insert(
        panel,
        show(
            move || open.get(),
            move || {
                let list = box_();
                insert(
                    list,
                    for_each(
                        move || (0..count.get().min(5)).collect::<Vec<i32>>(),
                        |i| *i,
                        |item, _| {
                            let row = text();
                            insert(row, View::dynamic(move || View::from(format!("row {}", item.get()))), At::Only);
                            View::Node(row)
                        },
                        None,
                    ),
                    At::Only,
                );
                View::Node(list)
            },
            None,
        ),
        At::End,
    );
    View::Node(root)
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async {
        renderer::create_cli_renderer(renderer::RendererConfig::default()).await.unwrap();
        let mode = renderer::wait_for_theme_mode(1000).await;
        let root = tree::root().unwrap();
        let _dispose = render(app, root);
        let _ = mode;
        renderer::request_render();
        renderer::closed().await;
    });
}
