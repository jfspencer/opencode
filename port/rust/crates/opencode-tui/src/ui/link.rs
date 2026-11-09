//! `ui/link.tsx`: `<Link>`, a hyperlink that opens in the default browser when clicked anywhere on its text.
//!
//! TODO(port-tui): `clickedLink(renderer, event)` (the hyperlink under a plain left click), which needs the
//! renderer's selection.
use std::rc::Rc;

use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{a, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::MouseEventType;

/// `LinkProps`.
pub struct LinkProps {
    pub href: String,
    /// The text shown; the URL itself when `None`.
    pub children: Option<String>,
    pub fg: Option<Rc<dyn Fn() -> RGBA>>,
    pub bg: Option<Rc<dyn Fn() -> RGBA>>,
    pub width: Option<Value>,
    pub wrap_mode: Option<WrapMode>,
}

impl LinkProps {
    pub fn new(href: impl Into<String>) -> LinkProps {
        LinkProps { href: href.into(), children: None, fg: None, bg: None, width: None, wrap_mode: None }
    }
}

/// `openUrl(input)` (`@opencode/util/open`): only http and https links open, with the platform's opener. The opener
/// is not waited on.
pub fn open_url(input: &str) -> Result<(), ()> {
    let lower = input.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(());
    }
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let mut child = std::process::Command::new(opener)
        .arg(input)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    // Reap the opener when it exits, so it leaves no zombie behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// `<Link href fg bg width wrapMode>{children}</Link>`.
pub fn link(props: LinkProps) -> View {
    let el = text();
    if let Some(fg) = props.fg {
        bind_always(move || fg(), move |c| el.set_fg(Some(c)));
    }
    if let Some(bg) = props.bg {
        bind_always(move || bg(), move |c| el.set_bg(Some(c)));
    }
    if let Some(width) = props.width {
        el.set_width(Some(width));
    }
    if let Some(mode) = props.wrap_mode {
        el.set_wrap_mode(mode);
    }
    let href = props.href.clone();
    el.set_on_mouse(
        Some(MouseEventType::Up),
        Some(Rc::new(move |_| {
            let _ = open_url(&href);
        })),
    );
    let anchor = a(&props.href);
    insert_node(anchor, text_node_from_string(props.children.as_deref().unwrap_or(&props.href)), None);
    insert_node(el, anchor, None);
    View::Node(el)
}
