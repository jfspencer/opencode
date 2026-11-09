//! `component/dialog-status.tsx`: the location's MCP servers and their status (MCP is dropped, so the list is the
//! empty one the server reports: "No MCP servers").
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_tui_core::elements::{b, box_, span, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::text::{WrapMode, text_node_from_string};
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::FlexDirection;

use crate::ctx::tui;
use crate::data::{array, get, get_str, object};
use crate::theme::SurfaceName;
use crate::ui::dialog::{HeaderProps, LayoutProps, dialog_header, dialog_layout};
use crate::ui::theme::use_theme;
use crate::util::rgba;

/// `<DialogStatus>`.
pub fn dialog_status() -> View {
    let data = tui().data.clone();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let mcp = create_memo(move || {
        let Some(directory) = data.directory.get() else { return Vec::new() };
        let location = object(vec![("directory", Json::String(directory))]);
        data.store.location.mcp.server.list(Some(&location)).map(|list| array(&list).to_vec()).unwrap_or_default()
    });
    let color = move |status: &str| {
        let t = theme.get();
        match status {
            "connected" => rgba(t.text.feedback.success.base),
            "failed" => rgba(t.text.feedback.error.base),
            "needs_auth" => rgba(t.text.feedback.warning.base),
            _ => rgba(t.text.muted),
        }
    };
    dialog_layout(LayoutProps {
        header: Box::new(|| {
            component(|| dialog_header(HeaderProps { title: Some(Rc::new(|| "Status".to_string())), ..HeaderProps::default() }))
        }),
        children: Box::new(move || {
            show(
                move || mcp.with(|m| !m.is_empty()),
                move || {
                    let el = box_();
                    let count = text();
                    bind_always(move || rgba(theme.get().text.base), move |c| count.set_fg(Some(c)));
                    insert(
                        count,
                        View::dynamic(move || {
                            let n = mcp.with(Vec::len);
                            View::from(format!("{n} MCP server{}", if n == 1 { "" } else { "s" }))
                        }),
                        At::Only,
                    );
                    insert_node(el, count, None);
                    insert(
                        el,
                        index(
                            move || mcp.get(),
                            move |item, _| {
                                let row = box_();
                                row.set_flex_direction(FlexDirection::Row);
                                row.set_gap(2, Some(Value::Point(1.0)));
                                let dot = text();
                                dot.set_flex_shrink(Some(0.0));
                                bind_always(
                                    move || color(item.with(|i| get(i, "status").and_then(|s| get_str(s, "status")).unwrap_or_default().to_string()).as_str()),
                                    move |c| dot.set_fg(Some(c)),
                                );
                                insert_node(dot, text_node_from_string("•"), None);
                                let line = text();
                                bind_always(move || rgba(theme.get().text.base), move |c| line.set_fg(Some(c)));
                                line.set_wrap_mode(WrapMode::Word);
                                let name = b();
                                insert(name, View::dynamic(move || View::from(item.with(|i| get_str(i, "name").unwrap_or_default().to_string()))), At::Only);
                                insert_node(line, name, None);
                                insert_node(line, text_node_from_string(" "), None);
                                let state = span();
                                bind_always(move || rgba(theme.get().text.muted), move |c| state.set_span_style(Some(c), None, 0, 0));
                                insert(
                                    state,
                                    View::dynamic(move || {
                                        View::from(item.with(|i| {
                                            let status = get(i, "status");
                                            let kind = status.and_then(|s| get_str(s, "status")).unwrap_or_default();
                                            let error = status.and_then(|s| get_str(s, "error")).unwrap_or_default();
                                            match kind {
                                                "connected" => "Connected".to_string(),
                                                "failed" => error.to_string(),
                                                "disabled" => "Disabled in configuration".to_string(),
                                                "needs_auth" => format!("Needs authentication: {error}"),
                                                other => other.to_string(),
                                            }
                                        }))
                                    }),
                                    At::Only,
                                );
                                insert_node(line, state, None);
                                insert_node(row, dot, None);
                                insert_node(row, line, None);
                                View::Node(row)
                            },
                            None,
                        ),
                        At::End,
                    );
                    View::Node(el)
                },
                Some(Rc::new(move || {
                    let line = text();
                    bind_always(move || rgba(theme.get().text.base), move |c| line.set_fg(Some(c)));
                    insert_node(line, text_node_from_string("No MCP servers"), None);
                    View::Node(line)
                })),
            )
        }),
        ..LayoutProps::default()
    })
}
