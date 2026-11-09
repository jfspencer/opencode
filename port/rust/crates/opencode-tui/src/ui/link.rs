//! `ui/link.tsx`: [`link`] (a `<text>` whose `<a>` opens its URL when clicked) and [`clicked_link`] (the hyperlink
//! under a plain left click, which App opens).
use std::rc::Rc;

use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{a, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::renderer;
use opencode_tui_core::renderables::text::{WrapMode, add_text_child, text_node_from_string};
use opencode_tui_core::selection::{self, SelectionBehavior};
use opencode_tui_core::solid::View;
use opencode_tui_core::tree::{MouseEvent, MouseEventType};

/// `LinkProps`.
#[derive(Clone, Default)]
pub struct LinkProps {
    pub href: String,
    /// The text shown (`children`); the URL when `None`.
    pub text: Option<String>,
    pub fg: Option<RGBA>,
    pub bg: Option<RGBA>,
    pub width: Option<Value>,
    pub wrap_mode: Option<WrapMode>,
}

/// `<Link>`: clicking anywhere on the text opens the URL in the default browser.
pub fn link(props: LinkProps) -> View {
    let el = text();
    el.set_fg(props.fg);
    el.set_bg(props.bg);
    if let Some(width) = props.width {
        el.set_width(Some(width));
    }
    if let Some(mode) = props.wrap_mode {
        el.set_wrap_mode(mode);
    }
    let href = props.href.clone();
    el.set_on_mouse(
        Some(MouseEventType::Up),
        Some(Rc::new(move |_event: &mut MouseEvent| {
            let _ = crate::util::open_url(&href);
        })),
    );
    let anchor = a(&props.href);
    add_text_child(el, anchor, None);
    add_text_child(anchor, text_node_from_string(props.text.as_deref().unwrap_or(&props.href)), None);
    View::Node(el)
}

/// `clickedLink(renderer, event)`: the hyperlink under a plain left click. OpenTUI marks the release that ends a
/// selection gesture with `isDragging`, and a fresh cell selection means the pointer never moved; other releases,
/// such as the end of a drag that started outside text, are not clicks. Every drawn cell stores its link's URL, so
/// a click on any row of a wrapped link resolves the whole link. Modified clicks are left to the terminal's own
/// hyperlink handling.
pub fn clicked_link(event: &MouseEvent) -> Option<String> {
    if !event.is_dragging || event.ctrl || event.alt || event.shift {
        return None;
    }
    let current = selection::get_selection()?;
    if !current.is_start() || current.behavior() != SelectionBehavior::Cell {
        return None;
    }
    renderer::link_at(event.x, event.y)
}
