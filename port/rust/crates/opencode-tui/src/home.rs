//! The home route (`routes/home.tsx`), the logo (`component/logo.tsx`, `logo.ts`) and the built-in home footer
//! plugin (`feature-plugins/home/footer.tsx`, appended to the `home.footer` slot).
//!
//! Plugin slots: only the built-in feature plugins are in scope, so a `<Slot path>` renders the built-in plugin's
//! content for that path directly (external TUI plugins are dropped).
use std::rc::Rc;

use opencode_tui_core::color::{RGBA, text_attributes};
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Align, Edge, FlexDirection};

use crate::ctx::tui;
use crate::prompt::history::PromptInfo;
use crate::prompt::{Placeholders, PromptProps, PromptRef, prompt};
use crate::util::rgba;

const LOGO_LEFT: [&str; 4] = ["                   ", "█▀▀█ █▀▀█ █▀▀█ █▀▀▄", "█__█ █__█ █^^^ █__█", "▀▀▀▀ █▀▀▀ ▀▀▀▀ ▀~~▀"];
const LOGO_RIGHT: [&str; 4] = ["             ▄     ", "█▀▀▀ █▀▀█ █▀▀█ █▀▀█", "█___ █__█ █__█ █^^^", "▀▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀"];
const GO_RIGHT: [&str; 4] = ["    ", "█▀▀█", "█__█", "▀▀▀▀"];

/// Home's prompt placeholders (`placeholder`).
const PLACEHOLDER_NORMAL: [&str; 3] = ["Fix a TODO in the codebase", "What is the tech stack of this project?", "Fix broken tests"];
const PLACEHOLDER_SHELL: [&str; 3] = ["ls -la", "git status", "pwd"];

thread_local! {
    /// `once`: --prompt was put in a home prompt.
    static ARGS_PROMPT_SET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `tint(base, overlay, alpha)` (theme/color.ts).
fn tint(base: RGBA, overlay: RGBA, alpha: f64) -> RGBA {
    RGBA::from_values(
        base.r() + (overlay.r() - base.r()) * alpha,
        base.g() + (overlay.g() - base.g()) * alpha,
        base.b() + (overlay.b() - base.b()) * alpha,
        base.a() + (overlay.a() - base.a()) * alpha,
    )
}

/// `renderLine(line, fg, bold)`: one text per character, the shadow for `_ ^ ~ ,`.
fn render_line(line: &str, fg: RGBA, bold: bool) -> View {
    let theme = tui().theme.get_untracked();
    let shadow = tint(rgba(theme.background.base), fg, 0.25);
    let attrs = if bold { Some(text_attributes::BOLD) } else { None };
    View::list(line.chars().map(|c| {
        let el = text();
        let (glyph, fg_color, bg) = match c {
            '_' => (" ".to_string(), fg, Some(shadow)),
            '^' => ("▀".to_string(), fg, Some(shadow)),
            '~' => ("▀".to_string(), shadow, None),
            ',' => ("▄".to_string(), shadow, None),
            other => (other.to_string(), fg, None),
        };
        insert_node(el, opencode_tui_core::renderables::text::text_node_from_string(&glyph), None);
        el.set_fg(Some(fg_color));
        if let Some(bg) = bg {
            el.set_bg(Some(bg));
        }
        el.set_attributes(attrs);
        el.set_selectable(false);
        View::Node(el)
    }))
}

fn row(content: impl Fn() -> View + 'static, gap: bool) -> View {
    let el = box_();
    el.set_flex_direction(FlexDirection::Row);
    if gap {
        el.set_gap(2, Some(Value::Point(1.0)));
    }
    insert(el, View::dynamic(content), At::Only);
    View::Node(el)
}

/// `<Logo>`.
pub fn logo() -> View {
    let t = tui();
    let dimensions = t.dimensions;
    let theme = t.theme;
    let root = box_();
    let small = create_memo(move || dimensions.get().1 < 12);
    let tiny = create_memo(move || dimensions.get().0 < 22);
    let narrow = create_memo(move || dimensions.get().0 < 44);
    insert(
        root,
        View::dynamic(move || {
            if small.get() {
                return View::Empty;
            }
            if tiny.get() {
                return for_each(
                    || GO_RIGHT[1..].to_vec(),
                    |line| *line,
                    move |line, _| row(move || render_line(line.get(), rgba(theme.get().text.base), true), false),
                    None,
                );
            }
            if narrow.get() {
                return View::list([
                    for_each(
                        || LOGO_LEFT[1..].to_vec(),
                        |line| *line,
                        move |line, _| row(move || render_line(line.get(), rgba(theme.get().text.muted), false), false),
                        None,
                    ),
                    for_each(
                        || LOGO_RIGHT.to_vec(),
                        |line| *line,
                        move |line, _| row(move || render_line(line.get(), rgba(theme.get().text.base), true), false),
                        None,
                    ),
                ]);
            }
            for_each(
                || LOGO_LEFT.to_vec(),
                |line| *line,
                move |line, index| {
                    let outer = box_();
                    outer.set_flex_direction(FlexDirection::Row);
                    outer.set_gap(2, Some(Value::Point(1.0)));
                    insert_node(outer, inner_row(move || render_line(line.get(), rgba(theme.get().text.muted), false)), None);
                    insert_node(
                        outer,
                        inner_row(move || render_line(LOGO_RIGHT[index.get()], rgba(theme.get().text.base), true)),
                        None,
                    );
                    View::Node(outer)
                },
                None,
            )
        }),
        At::Only,
    );
    View::Node(root)
}

fn inner_row(content: impl Fn() -> View + 'static) -> opencode_tui_core::tree::Element {
    let el = box_();
    el.set_flex_direction(FlexDirection::Row);
    insert(el, View::dynamic(content), At::Only);
    el
}

/// `homeFooterVisibility(width)`.
fn version_visible(width: u32) -> bool {
    width >= 64
}

/// The home footer plugin's `View`.
fn home_footer() -> View {
    let t = tui();
    let dimensions = t.dimensions;
    let theme = t.theme;
    let version = t.app.version.clone();
    show(
        move || dimensions.get().1 >= 14 && dimensions.get().0 >= 44,
        move || {
            let el = box_();
            el.set_width(Some(Value::Percent(100.0)));
            bind(move || if dimensions.get().1 < 16 { 0.0 } else { 1.0 }, move |p| {
                el.set_padding(Edge::Top, Some(Value::Point(p)));
                el.set_padding(Edge::Bottom, Some(Value::Point(p)));
            });
            el.set_padding(Edge::Left, Some(Value::Point(2.0)));
            el.set_padding(Edge::Right, Some(Value::Point(2.0)));
            el.set_flex_direction(FlexDirection::Row);
            el.set_flex_shrink(Some(0.0));
            el.set_gap(2, Some(Value::Point(2.0)));
            let spacer = box_();
            insert_node(el, spacer, None);
            spacer.set_flex_grow(Some(1.0));
            let version = version.clone();
            insert(
                el,
                show(
                    move || version_visible(dimensions.get().0),
                    move || {
                        let wrapper = box_();
                        wrapper.set_flex_shrink(Some(0.0));
                        let label = text();
                        insert_node(wrapper, label, None);
                        bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| label.set_fg(Some(c)));
                        insert(label, View::from(version.clone()), At::Only);
                        View::Node(wrapper)
                    },
                    None,
                ),
                At::End,
            );
            View::Node(el)
        },
        None,
    )
}

/// `<Home>`.
pub fn home() -> View {
    let t = tui();
    let dimensions = t.dimensions;
    let (logo_width, set_logo_width) = create_signal(0.0f32);
    let _ = logo_width;
    let root = box_();
    let b2 = box_();
    let b3 = box_();
    let b4 = box_();
    let b5 = box_();
    let b6 = box_();
    let b7 = box_();
    for child in [b2, b3, b4, b5, b6, b7] {
        insert_node(root, child, None);
    }
    root.set_flex_grow(Some(1.0));
    root.set_align_items(Align::Center);
    b2.set_flex_grow(Some(1.0));
    b2.set_min_height(Some(Value::Point(0.0)));
    b3.set_height(Some(Value::Point(3.0)));
    b3.set_min_height(Some(Value::Point(0.0)));
    b3.set_flex_shrink(Some(1.0));
    b4.set_flex_shrink(Some(0.0));
    b4.set_on_size_change(Some(Rc::new(move || set_logo_width.set(b4.width()))));
    insert(b4, component(logo), At::Only);
    b5.set_height(Some(Value::Point(1.0)));
    b5.set_flex_shrink(Some(0.0));
    b6.set_width(Some(Value::Percent(100.0)));
    b6.set_max_width(Some(Value::Point(75.0)));
    b6.set_z_index(1000);
    b6.set_padding(Edge::Top, Some(Value::Point(1.0)));
    b6.set_flex_shrink(Some(0.0));
    b6.set_position_type(false);
    // The location context follows the route's location, else the default one (tracked alone: `location.set`
    // reads other signals). TODO(port-tui): `route.location` (Home has none yet).
    {
        let data = t.data.clone();
        create_effect(move || {
            let target = data.store.location.default();
            untrack(|| data.location.set(Some(target)));
        });
    }
    let (prompt_ref, set_prompt_ref) = create_signal_with(None::<PromptRef>, None);
    {
        let args_prompt = t.args.prompt.clone();
        // `bind`: --prompt goes into the first home prompt, once.
        let bind_ref = move |r: Option<PromptRef>| {
            set_prompt_ref.set(r.clone());
            let Some(r) = r else { return };
            let Some(text) = args_prompt.clone() else { return };
            if ARGS_PROMPT_SET.with(|once| once.replace(true)) {
                return;
            }
            r.set(&PromptInfo { text, files: Some(vec![]), agents: Some(vec![]), skills: None, pasted: vec![], mode: None });
        };
        insert(
            b6,
            component(move || {
                prompt(PromptProps {
                    session_id: None,
                    visible: None,
                    // TODO(port-tui): `forms().length > 0` (global forms on home).
                    disabled: Rc::new(|| false),
                    muted: None,
                    on_submit: None,
                    on_empty_submit: None,
                    on_ref: Rc::new(bind_ref.clone()),
                    placeholders: Placeholders {
                        normal: PLACEHOLDER_NORMAL.iter().map(|s| s.to_string()).collect(),
                        shell: PLACEHOLDER_SHELL.iter().map(|s| s.to_string()).collect(),
                    },
                })
            }),
            At::Only,
        );
    }
    // Wait for everything submit needs before auto-submitting --prompt; it runs once.
    {
        let local = crate::local::local();
        let args_prompt = t.args.prompt.clone();
        let sent = std::cell::Cell::new(false);
        create_effect(move || {
            let Some(r) = prompt_ref.get() else { return };
            if sent.get() {
                return;
            }
            // TODO(port-tui): `local.model.ready` (the model preferences).
            if !local.catalog_ready() {
                return;
            }
            if local.agent_current().is_none() || local.model_selection().is_none() {
                return;
            }
            let Some(text) = args_prompt.as_deref() else { return };
            if r.current_tracked().text != text {
                return;
            }
            sent.set(true);
            r.submit();
        });
    }
    b7.set_flex_grow(Some(1.0));
    b7.set_min_height(Some(Value::Point(0.0)));
    bind(move || if dimensions.get().0 < 44 { 1.0 } else { 2.0 }, move |p| {
        root.set_padding(Edge::Left, Some(Value::Point(p)));
        root.set_padding(Edge::Right, Some(Value::Point(p)));
    });

    let footer = box_();
    footer.set_width(Some(Value::Percent(100.0)));
    footer.set_flex_shrink(Some(0.0));
    insert(footer, home_footer(), At::Only);
    View::list([View::Node(root), View::Node(footer)])
}
