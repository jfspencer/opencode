//! The JSX intrinsic elements (`@opentui/solid`'s `src/elements/catalogue.ts`): each creates its renderable as
//! `createElement(tag)` does, with only an id; props are then set one by one (the typed setters on
//! [`Element`]).
use crate::color::text_attributes;
use crate::renderables::{box_, code, markdown, text, text_table, textarea};
use crate::tree::Element;

/// `<box>`.
pub fn box_() -> Element {
    box_::create_box()
}

/// `<text>`.
pub fn text() -> Element {
    text::create_text()
}

/// `<span>`.
pub fn span() -> Element {
    text::create_text_node(text::TextNodeKind::default(), "span")
}

fn modifier(attributes: u32, prefix: &str) -> Element {
    text::create_text_node(text::TextNodeKind { attributes, ..Default::default() }, prefix)
}

/// `<b>` / `<strong>`.
pub fn b() -> Element {
    modifier(text_attributes::BOLD, "b")
}

/// `<i>` / `<em>`.
pub fn i() -> Element {
    modifier(text_attributes::ITALIC, "i")
}

/// `<u>`.
pub fn u() -> Element {
    modifier(text_attributes::UNDERLINE, "u")
}

/// `<br>`: a text node holding a newline.
pub fn br() -> Element {
    text::create_text_node(text::TextNodeKind { text: Some("\n".into()), ..Default::default() }, "br")
}

/// `<a href>`: a span with a link.
pub fn a(href: &str) -> Element {
    text::create_text_node(text::TextNodeKind { link: Some(href.to_string()), ..Default::default() }, "a")
}

/// `<textarea>`.
pub fn textarea() -> Element {
    textarea::create_textarea()
}

/// `<code>`.
pub fn code() -> Element {
    code::create_code()
}

/// `<markdown>`.
pub fn markdown() -> Element {
    markdown::create_markdown()
}

/// `<text_table>`.
pub fn text_table() -> Element {
    text_table::create_text_table()
}
