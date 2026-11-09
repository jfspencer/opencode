//! `ui/dialog-help.tsx`.
use std::rc::Rc;

use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Edge, FlexDirection, Justify, MouseEventType};

use crate::keymap::{Command, Layer, create_layer, use_shortcuts};
use crate::theme::SurfaceName;
use crate::ui::dialog::{FooterAction, FooterActions, FooterProps, HeaderProps, LayoutProps, dialog_header, dialog_layout, use_dialog};
use crate::ui::theme::use_theme;
use crate::util::rgba;

/// `<DialogHelp>`.
pub fn dialog_help() -> View {
    let dialog = use_dialog();
    let theme = use_theme().surface(SurfaceName::Dialog);
    let shortcuts = use_shortcuts();
    {
        let dialog = dialog.clone();
        create_layer(move || {
            let enter = dialog.clone();
            let escape = dialog.clone();
            Layer {
                mode: Some("modal"),
                commands: vec![
                    Command::inline("return", move || {
                        enter.clear();
                        true
                    })
                    .title("Close help")
                    .group("Dialog"),
                    Command::inline("escape", move || {
                        escape.clear();
                        true
                    })
                    .title("Close help")
                    .group("Dialog"),
                ],
                ..Layer::default()
            }
        });
    }
    let ok = dialog.clone();
    let legacy_close = dialog.clone();
    dialog_layout(LayoutProps {
        header: Box::new(|| {
            component(|| {
                dialog_header(HeaderProps {
                    title: Some(Rc::new(|| "Help".to_string())),
                    close_label: Some("esc/enter".into()),
                    ..HeaderProps::default()
                })
            })
        }),
        footer: Some(FooterProps {
            actions: Some(Rc::new(move || {
                let ok = ok.clone();
                FooterActions {
                    secondary: Vec::new(),
                    primary: vec![FooterAction {
                        title: "ok".into(),
                        key: Some("enter".into()),
                        on_trigger: Some(Rc::new(move || ok.clear())),
                        ..FooterAction::default()
                    }],
                }
            })),
            ..FooterProps::default()
        }),
        legacy_footer: Some(Rc::new(move || {
            let row = box_();
            row.set_flex_direction(FlexDirection::Row);
            row.set_justify_content(Justify::FlexEnd);
            let button = box_();
            insert_node(row, button, None);
            button.set_padding(Edge::Left, Some(Value::Point(3.0)));
            button.set_padding(Edge::Right, Some(Value::Point(3.0)));
            bind_always(move || rgba(theme.get().background.action.primary.focused), move |c| button.set_background_color(Some(c)));
            let close = legacy_close.clone();
            button.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| close.clear())));
            let label = text();
            bind_always(move || rgba(theme.get().text.action.primary.focused), move |c| label.set_fg(Some(c)));
            insert_node(label, text_node_from_string("ok"), None);
            insert_node(button, label, None);
            View::Node(row)
        })),
        children: Box::new(move || {
            let line = text();
            bind_always(move || rgba(theme.get().text.muted), move |c| line.set_fg(Some(c)));
            let tail = text_node_from_string(" to see all available actions and commands in any context.");
            insert_node(line, text_node_from_string("Press "), None);
            insert_node(line, tail, None);
            insert(
                line,
                View::dynamic(move || View::from(shortcuts.get("command.palette.show").unwrap_or_default())),
                At::Before(tail),
            );
            View::Node(line)
        }),
        ..LayoutProps::default()
    })
}
