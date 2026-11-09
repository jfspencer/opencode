//! `routes/session/message-navigation.ts`: the next or previous message boundary, and the bottom slack that lets
//! the last messages align to the top.
use std::collections::HashMap;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_sync::json::JsonExt;

/// `messageNavigationSlack({ top, viewportHeight, scrollHeight, currentSlack })`.
pub fn message_navigation_slack(top: f64, viewport_height: f64, scroll_height: f64, current_slack: f64) -> f64 {
    let content_height = scroll_height - current_slack;
    (top + viewport_height - content_height).ceil().max(0.0)
}

/// A navigation target: the message, its content row, and where it aligns.
#[derive(Clone, Debug, PartialEq)]
pub struct Boundary {
    pub id: String,
    pub y: f64,
    pub top: f64,
}

/// `findMessageBoundary({ direction, children, messages, scrollTop, viewportY, currentID, userOnly })`.
pub fn find_message_boundary(
    next: bool,
    children: &[(String, i32)],
    messages: &[Rc<Json>],
    scroll_top: f64,
    viewport_y: f64,
    current_id: Option<&str>,
    user_only: bool,
) -> Option<Boundary> {
    let by_id: HashMap<&str, &Rc<Json>> = messages.iter().map(|m| (m.str("id").unwrap_or_default(), m)).collect();
    let mut visible: Vec<Boundary> = children
        .iter()
        .filter_map(|(id, child_y)| {
            let message = by_id.get(id.as_str())?;
            let y = scroll_top + *child_y as f64 - viewport_y;
            let kind = message.str("type");
            if kind == Some("user") && !message.str("text").unwrap_or_default().trim().is_empty() {
                return Some(Boundary { id: id.clone(), y, top: y });
            }
            if user_only || kind != Some("assistant") {
                return None;
            }
            let has_text = message.arr("content").is_some_and(|content| {
                content.iter().any(|c| c.str("type") == Some("text") && !c.str("text").unwrap_or_default().trim().is_empty())
            });
            if !has_text {
                return None;
            }
            Some(Boundary { id: id.clone(), y, top: (y - 1.0).max(0.0) })
        })
        .collect();
    visible.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(current) = visible.iter().position(|c| Some(c.id.as_str()) == current_id) {
        let index = if next { current.checked_add(1) } else { current.checked_sub(1) };
        return index.and_then(|i| visible.get(i).cloned());
    }
    if next {
        return visible.into_iter().find(|c| c.y > scroll_top);
    }
    visible.into_iter().rev().find(|c| c.y < scroll_top)
}
