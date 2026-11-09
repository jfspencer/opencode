//! `component/reconnecting.tsx` (`<Reconnecting managed>`), and App's overlay timing (app.tsx: the overlay shows
//! after 1 s of reconnecting, or 5 s of a first connection, and hides once connected).
use std::cell::Cell;
use std::rc::Rc;

use opencode_tui_core::color::RGBA;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Align, Edge, FlexDirection, Justify};
use opencode_tui_sync::connection::Status;

use crate::ctx::tui;
use crate::spinner::{SpinnerProps, spinner};
use crate::util::rgba;

/// `<Reconnecting managed>`.
pub fn reconnecting(managed: bool) -> View {
    let theme = tui().theme;
    let overlay = box_();
    overlay.set_position_type(true);
    overlay.set_z_index(10_000);
    overlay.set_positions(Some(Some(Value::Point(0.0))), Some(Some(Value::Point(0.0))), Some(Some(Value::Point(0.0))), Some(Some(Value::Point(0.0))));
    overlay.set_background_color(Some(RGBA::from_ints(0.0, 0.0, 0.0, 150.0)));
    overlay.set_align_items(Align::Center);
    overlay.set_justify_content(Justify::Center);
    let panel = box_();
    insert_node(overlay, panel, None);
    panel.set_width(Some(Value::Point(48.0)));
    panel.set_max_width(Some(Value::Percent(90.0)));
    panel.set_flex_direction(FlexDirection::Column);
    bind_always(move || rgba(theme.with(|th| th.background.raised.base)), move |c| panel.set_background_color(Some(c)));
    panel.set_padding(Edge::Top, Some(Value::Point(1.0)));
    panel.set_padding(Edge::Bottom, Some(Value::Point(1.0)));
    panel.set_padding(Edge::Left, Some(Value::Point(2.0)));
    panel.set_padding(Edge::Right, Some(Value::Point(2.0)));
    panel.set_gap(2, Some(Value::Point(1.0)));
    insert(
        panel,
        spinner(SpinnerProps {
            children: Some(Rc::new(move || View::from(if managed { "Restarting service…" } else { "Connection lost…" }))),
            color: Some(Rc::new(move || Some(rgba(theme.with(|th| th.text.base))))),
            shimmer: None,
        }),
        At::End,
    );
    let note = text();
    insert_node(panel, note, None);
    bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| note.set_fg(Some(c)));
    insert(
        note,
        View::from(if managed { "Your session will resume automatically." } else { "Reconnecting to the server automatically." }),
        At::Only,
    );
    View::Node(overlay)
}

/// App's `showReconnecting` and its `<Show>`: suppresses the overlay for transient startup and retry states.
/// TODO(port-tui): `client.restart` (a managed service) is never set for `--server`.
pub fn reconnecting_overlay() -> View {
    let t = tui();
    let connection = t.data.connection.clone();
    let (show_overlay, set_show) = create_signal(false);
    let timer: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
    {
        let timer = timer.clone();
        create_effect(move || {
            if let Some(id) = timer.take() {
                renderer::clear_timeout(id);
            }
            let status = connection.status();
            if status == Status::Connected {
                set_show.set(false);
                return;
            }
            let delay = if status == Status::Reconnecting { 1000 } else { 5000 };
            let slot = timer.clone();
            timer.set(Some(renderer::set_timeout(delay, move || {
                slot.set(None);
                set_show.set(true);
            })));
        });
    }
    on_cleanup(move || {
        if let Some(id) = timer.take() {
            renderer::clear_timeout(id);
        }
    });
    show(move || show_overlay.get(), || reconnecting(false), None)
}
