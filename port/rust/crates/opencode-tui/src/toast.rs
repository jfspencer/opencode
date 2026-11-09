//! Toasts (`ui/toast.tsx`): the toast service (`ToastProvider`/`useToast`: show, error, pause, resume, dismiss,
//! activate, the queue) and `<Toast>`, the surface App draws over everything at the top right.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use opencode_tui_core::border::{BorderCharacters, border_chars_to_array};
use opencode_tui_core::color::text_attributes;
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderables::box_::{BorderProp, Side};
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{Align, Edge, FlexDirection, Justify, MouseEventType};

use crate::util::rgba;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Info,
    Success,
    Warning,
    Error,
}

/// `ToastOptions`.
#[derive(Clone)]
pub struct ToastOptions {
    pub title: Option<String>,
    pub message: String,
    pub variant: Variant,
    pub duration: u64,
    pub action: Option<(String, Rc<dyn Fn()>)>,
}

impl PartialEq for ToastOptions {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

/// `ToastInput`: options with an optional duration (5000 ms by default).
pub struct ToastInput {
    pub title: Option<String>,
    pub message: String,
    pub variant: Variant,
    pub duration: Option<u64>,
    pub action: Option<(String, Rc<dyn Fn()>)>,
}

impl ToastInput {
    pub fn new(variant: Variant, message: impl Into<String>) -> ToastInput {
        ToastInput { title: None, message: message.into(), variant, duration: None, action: None }
    }
    pub fn duration(mut self, ms: u64) -> ToastInput {
        self.duration = Some(ms);
        self
    }
    pub fn title(mut self, title: impl Into<String>) -> ToastInput {
        self.title = Some(title.into());
        self
    }
}

struct State {
    timeout: Cell<Option<u64>>,
    started_at: Cell<std::time::Instant>,
    remaining: Cell<u64>,
    paused: Cell<bool>,
}

/// The toast service (`useToast()`).
#[derive(Clone)]
pub struct Toasts {
    current: Signal<Option<Rc<ToastOptions>>>,
    queue: Signal<Vec<Rc<ToastOptions>>>,
    state: Rc<State>,
}

impl Toasts {
    /// `init()`, in the provider's owner.
    pub fn new() -> Toasts {
        let toasts = Toasts {
            current: create_signal_with(None, None).0,
            queue: create_signal_with(Vec::new(), None).0,
            state: Rc::new(State {
                timeout: Cell::new(None),
                started_at: Cell::new(std::time::Instant::now()),
                remaining: Cell::new(0),
                paused: Cell::new(false),
            }),
        };
        let state = toasts.state.clone();
        on_cleanup(move || {
            if let Some(id) = state.timeout.take() {
                renderer::clear_timeout(id);
            }
        });
        toasts
    }

    fn clear(&self) {
        if let Some(id) = self.state.timeout.take() {
            renderer::clear_timeout(id);
        }
    }

    fn start(&self, duration: u64) {
        self.clear();
        self.state.remaining.set(duration);
        self.state.started_at.set(std::time::Instant::now());
        let this = self.clone();
        self.state.timeout.set(Some(renderer::set_timeout(duration, move || this.dismiss())));
    }

    /// `toast.show(options)`.
    pub fn show(&self, input: ToastInput) {
        let options = Rc::new(ToastOptions {
            title: input.title,
            message: input.message,
            variant: input.variant,
            duration: input.duration.unwrap_or(5000),
            action: input.action,
        });
        let busy = self.current.with_untracked(Option::is_some)
            && (self.state.paused.get() || self.queue.with_untracked(|q| !q.is_empty()));
        if busy {
            self.queue.update(|q| q.push(options));
            return;
        }
        let duration = options.duration;
        self.current.set(Some(options));
        self.start(duration);
    }

    /// `toast.error(err)`.
    pub fn error(&self, message: impl Into<String>) {
        self.show(ToastInput::new(Variant::Error, message));
    }

    pub fn pause(&self) {
        if self.current.with_untracked(Option::is_none) || self.state.paused.get() {
            return;
        }
        self.state.paused.set(true);
        let elapsed = self.state.started_at.get().elapsed().as_millis() as u64;
        self.state.remaining.set(self.state.remaining.get().saturating_sub(elapsed));
        self.clear();
    }

    pub fn resume(&self) {
        if self.current.with_untracked(Option::is_none) || !self.state.paused.get() {
            return;
        }
        self.state.paused.set(false);
        self.start(self.state.remaining.get());
    }

    pub fn dismiss(&self) {
        self.clear();
        let next = self.queue.with_untracked(|q| q.first().cloned());
        batch(|| {
            self.queue.update(|q| {
                if !q.is_empty() {
                    q.remove(0);
                }
            });
            self.current.set(next.clone());
        });
        match next {
            None => self.state.paused.set(false),
            Some(next) => {
                self.state.remaining.set(next.duration);
                if !self.state.paused.get() {
                    self.start(next.duration);
                }
            }
        }
    }

    pub fn activate(&self) {
        let action = self.current.with_untracked(|c| c.as_ref().and_then(|t| t.action.clone()));
        self.dismiss();
        if let Some((_, run)) = action {
            run();
        }
    }

    pub fn current(&self) -> Option<Rc<ToastOptions>> {
        self.current.get()
    }

    pub fn pending(&self) -> usize {
        self.queue.with(Vec::len)
    }
}

impl Default for Toasts {
    fn default() -> Self {
        Toasts::new()
    }
}

const SPLIT: BorderCharacters = BorderCharacters {
    top_left: "",
    top_right: "",
    bottom_left: "",
    bottom_right: "",
    horizontal: " ",
    vertical: "┃",
    top_t: "",
    bottom_t: "",
    left_t: "",
    right_t: "",
    cross: "",
};

/// `<ToastSurface>`.
fn surface(toast: Rc<ToastOptions>, toasts: Toasts) -> View {
    let t = crate::ctx::tui();
    let theme = t.theme;
    let dimensions = t.dimensions;
    let (hovered, set_hovered) = create_signal(false);
    let hover = {
        let toasts = toasts.clone();
        move |value: bool| {
            set_hovered.set(value);
            if value { toasts.pause() } else { toasts.resume() }
        }
    };
    let affordance = {
        let toast = toast.clone();
        move || {
            let el = text();
            el.set_flex_shrink(Some(0.0));
            el.set_margin(Edge::Left, Some(Value::Point(2.0)));
            el.set_wrap_mode(WrapMode::None);
            let has_action = toast.action.is_some();
            bind(move || hovered.get() && has_action, move |bold| el.set_attributes(bold.then_some(text_attributes::BOLD)));
            bind_always(
                move || rgba(theme.with(|th| if hovered.get() { th.text.action.primary.base } else { th.text.muted })),
                move |c| el.set_fg(Some(c)),
            );
            let label = match &toast.action {
                Some((label, _)) => format!("› {label}"),
                None => "x".to_string(),
            };
            insert(el, View::from(label), At::Only);
            el
        }
    };
    let root = box_();
    root.set_position_type(true);
    root.set_positions(None, Some(Some(Value::Point(1.0))), Some(Some(Value::Point(2.0))), None);
    bind(move || (dimensions.get().0 as f32 - 6.0).min(60.0), move |w| root.set_max_width(Some(Value::Point(w))));
    root.set_justify_content(Justify::Center);
    root.set_align_items(Align::FlexStart);
    let variant = toast.variant;
    bind_always(
        move || {
            rgba(theme.with(|th| match variant {
                Variant::Info => th.text.feedback.info.base,
                Variant::Success => th.text.feedback.success.base,
                Variant::Warning => th.text.feedback.warning.base,
                Variant::Error => th.text.feedback.error.base,
            }))
        },
        move |c| root.set_border_color(Some(c)),
    );
    root.set_border(BorderProp::Sides(vec![Side::Left, Side::Right]));
    root.set_custom_border_chars(Some(border_chars_to_array(&SPLIT)));
    {
        let hover = hover.clone();
        root.set_on_mouse(Some(MouseEventType::Over), Some(Rc::new(move |_| hover(true))));
    }
    root.set_on_mouse(Some(MouseEventType::Out), Some(Rc::new(move |_| hover(false))));
    {
        let toasts = toasts.clone();
        root.set_on_mouse(Some(MouseEventType::Up), Some(Rc::new(move |_| toasts.activate())));
    }
    let body = box_();
    insert_node(root, body, None);
    body.set_width(Some(Value::Percent(100.0)));
    body.set_padding(Edge::Horizontal, Some(Value::Point(2.0)));
    body.set_padding(Edge::Vertical, Some(Value::Point(1.0)));
    bind_always(move || rgba(theme.with(|th| th.background.raised.high)), move |c| body.set_background_color(Some(c)));
    let message = toast.message.clone();
    match toast.title.clone() {
        None => {
            let row = box_();
            insert_node(body, row, None);
            row.set_flex_direction(FlexDirection::Row);
            row.set_width(Some(Value::Percent(100.0)));
            let label = text();
            insert_node(row, label, None);
            bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| label.set_fg(Some(c)));
            label.set_wrap_mode(WrapMode::Word);
            label.set_flex_grow(Some(1.0));
            insert(label, View::from(message), At::Only);
            insert_node(row, affordance(), None);
        }
        Some(title) => {
            let row = box_();
            insert_node(body, row, None);
            row.set_flex_direction(FlexDirection::Row);
            row.set_width(Some(Value::Percent(100.0)));
            row.set_margin(Edge::Bottom, Some(Value::Point(1.0)));
            let heading = text();
            insert_node(row, heading, None);
            heading.set_attributes(Some(text_attributes::BOLD));
            bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| heading.set_fg(Some(c)));
            insert(heading, View::from(title), At::Only);
            let spacer = box_();
            insert_node(row, spacer, None);
            spacer.set_flex_grow(Some(1.0));
            insert_node(row, affordance(), None);
            let label = text();
            insert_node(body, label, None);
            bind_always(move || rgba(theme.with(|th| th.text.base)), move |c| label.set_fg(Some(c)));
            label.set_wrap_mode(WrapMode::Word);
            label.set_width(Some(Value::Percent(100.0)));
            insert(label, View::from(message), At::Only);
        }
    }
    let pending_toasts = toasts.clone();
    insert(
        body,
        show(
            move || pending_toasts.pending() > 0,
            move || {
                let el = text();
                bind_always(move || rgba(theme.with(|th| th.text.muted)), move |c| el.set_fg(Some(c)));
                el.set_margin(Edge::Top, Some(Value::Point(1.0)));
                let toasts = toasts.clone();
                insert(el, View::dynamic(move || View::from(format!("+{} more", toasts.pending()))), At::Only);
                View::Node(el)
            },
            None,
        ),
        At::End,
    );
    let _ = RefCell::new(());
    View::Node(root)
}

/// `<Toast>`.
pub fn toast_view() -> View {
    let toasts = crate::ctx::tui().toast.clone();
    let current = toasts.clone();
    show_keyed(move || current.current(), move |toast| surface(toast, toasts.clone()), None)
}
