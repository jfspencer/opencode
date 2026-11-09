//! `util/todo.ts` (todos, their markers, a session's current list) and `component/todo-item.tsx`.
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::FlexDirection;

use super::display::{field, string_value};
use crate::theme::ResolvedTheme;
use crate::util::rgba;

/// `TodoStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

/// `Todo`.
#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub content: String,
    pub status: TodoStatus,
}

/// `parseTodos(value)`.
pub fn parse_todos(value: Option<&Json>) -> Vec<Todo> {
    let Some(Json::Array(items)) = value else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let content = string_value(field(Some(item), "content")).filter(|c| !c.is_empty())?;
            let status = match string_value(field(Some(item), "status")) {
                Some("in_progress") => TodoStatus::InProgress,
                Some("completed") => TodoStatus::Completed,
                Some("cancelled") => TodoStatus::Cancelled,
                _ => TodoStatus::Pending,
            };
            Some(Todo { content: content.to_string(), status })
        })
        .collect()
}

/// `todoMarker(status, ascii)`.
pub fn todo_marker(status: TodoStatus, ascii: bool) -> &'static str {
    match status {
        TodoStatus::Completed => {
            if ascii {
                "[x]"
            } else {
                "[✓]"
            }
        }
        TodoStatus::InProgress => {
            if ascii {
                "[>]"
            } else {
                "[•]"
            }
        }
        TodoStatus::Cancelled => "[-]",
        TodoStatus::Pending => "[ ]",
    }
}

/// `sessionTodos(messages, boundary)`: the input of the latest completed `todowrite` before the boundary.
pub fn session_todos(messages: &[Rc<Json>], boundary: Option<&str>) -> Vec<Todo> {
    let end = boundary
        .and_then(|b| messages.iter().position(|m| string_value(field(Some(m), "id")) == Some(b)))
        .unwrap_or(messages.len());
    let tool = messages[..end]
        .iter()
        .filter(|m| string_value(field(Some(m), "type")) == Some("assistant"))
        .flat_map(|m| match field(Some(m), "content") {
            Some(Json::Array(items)) => items.iter().collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .filter(|item| {
            string_value(field(Some(item), "type")) == Some("tool")
                && string_value(field(Some(item), "name")) == Some("todowrite")
                && string_value(field(field(Some(item), "state"), "status")) == Some("completed")
        })
        .last();
    parse_todos(field(field(field(tool, "state"), "input"), "todos"))
}

/// `<TodoItem theme todo>`.
pub fn todo_item(theme: Signal<ResolvedTheme>, todo: Rc<dyn Fn() -> Todo>) -> View {
    let fg = {
        let todo = todo.clone();
        move || {
            theme.with(|t| match todo().status {
                TodoStatus::InProgress => rgba(t.text.feedback.warning.base),
                TodoStatus::Cancelled => rgba(t.text.faint),
                _ => rgba(t.text.muted),
            })
        }
    };
    let row = box_();
    row.set_flex_direction(FlexDirection::Row);
    row.set_gap(2, Some(opencode_tui_core::layout::Value::Point(0.0)));
    let marker = text();
    let content = text();
    insert_node(row, marker, None);
    insert_node(row, content, None);
    marker.set_flex_shrink(Some(0.0));
    {
        let fg = fg.clone();
        bind_always(fg, move |c| marker.set_fg(Some(c)));
    }
    {
        let todo = todo.clone();
        insert(marker, View::dynamic(move || View::from(todo_marker(todo().status, false))), At::End);
        insert(marker, View::from(" "), At::End);
    }
    content.set_flex_grow(Some(1.0));
    content.set_wrap_mode(WrapMode::Word);
    bind_always(fg, move |c| content.set_fg(Some(c)));
    {
        let todo = todo.clone();
        bind(
            move || todo().status == TodoStatus::Cancelled,
            move |cancelled| content.set_attributes(cancelled.then_some(text_attributes::STRIKETHROUGH)),
        );
    }
    insert(content, View::dynamic(move || View::from(todo().content)), At::Only);
    View::Node(row)
}
