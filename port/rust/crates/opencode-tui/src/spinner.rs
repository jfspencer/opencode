//! `component/spinner.tsx` (`<Spinner>`), `spinner-frames.ts` and `spinner-renderable.ts` (the `<spinner>` element:
//! frames of graphemes, advanced by the renderer's shared animation clock).
//!
//! TODO(port-tui): `<ShimmerText>` (shimmer-text.tsx over masked-text.ts): only `<Spinner shimmer>` uses it, which
//! only the update dialog passes; it draws as the plain spinner here.
use std::any::Any;
use std::rc::Rc;

use opencode_tui_core::buffer::OptimizedBuffer;
use opencode_tui_core::color::{RGBA, parse_color};
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::renderables::text::text_node_from_string;
use opencode_tui_core::solid::*;
use opencode_tui_core::tree::{self, Element, FlexDirection, Kind};

use crate::ctx::tui;
use crate::util::{graphemes, rgba, string_width};

/// `SPINNER_FRAMES`.
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// `SpinnerRenderable`.
pub struct SpinnerKind {
    frames: Vec<String>,
    glyphs: Vec<Vec<(String, u32)>>,
    frame_width: u32,
    interval: f64,
    autoplay: bool,
    color: RGBA,
    background: RGBA,
    index: usize,
}

impl Kind for SpinnerKind {
    fn name(&self) -> &'static str {
        "spinner"
    }
    fn render(&mut self, _el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, _width: u32, _height: u32) {
        let Some(glyphs) = self.glyphs.get(self.index) else { return };
        let mut x = x;
        for (char, width) in glyphs {
            buffer.draw_text(char, x, y, self.color, Some(self.background), 0, None);
            x += *width as i32;
        }
    }
    fn on_animation_tick(&mut self, el: Element, _delta: f64, ticks: u64, _time: f64) {
        if !self.glyphs.is_empty() {
            self.index = (self.index + ticks as usize) % self.glyphs.len();
        }
        el.request_render();
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl SpinnerKind {
    /// `restart()`: `animationFps` while autoplay is on and there is more than one frame.
    fn restart(&self, el: Element) {
        let fps = (self.autoplay && self.glyphs.len() > 1).then(|| 1000.0 / self.interval);
        el.set_animation_fps(fps);
        el.request_render();
    }
}

/// `new SpinnerRenderable(ctx, { id })` with the default options (frames, interval 80, autoplay).
pub fn create_spinner() -> Element {
    let kind = SpinnerKind {
        frames: Vec::new(),
        glyphs: Vec::new(),
        frame_width: 0,
        interval: 80.0,
        autoplay: true,
        color: RGBA::from_values(1.0, 1.0, 1.0, 1.0),
        background: parse_color("transparent"),
        index: 0,
    };
    let el = tree::create(Box::new(kind), "spinner", false);
    let measured = el;
    el.set_measure_func(Some(Box::new(move |_, _, _, _| {
        let width = measured.with_kind::<SpinnerKind, _>(|k| k.frame_width).unwrap_or(0);
        (width as f32, 1.0)
    })));
    el.set_spinner_frames(SPINNER_FRAMES.iter().map(|f| f.to_string()).collect());
    el
}

/// The spinner's setters.
pub trait SpinnerElement {
    fn set_spinner_frames(self, frames: Vec<String>);
    fn set_spinner_interval(self, interval: f64);
    fn set_spinner_color(self, color: RGBA);
}

impl SpinnerElement for Element {
    /// `frames = value`.
    fn set_spinner_frames(self, frames: Vec<String>) {
        let remeasure = self.with_kind::<SpinnerKind, _>(|k| {
            if k.frames == frames {
                return false;
            }
            k.glyphs = frames
                .iter()
                .map(|f| graphemes(f).into_iter().map(|g| (g.to_string(), string_width(g))).collect())
                .collect();
            k.frames = frames;
            k.index = 0;
            let width = k.glyphs.iter().map(|g| g.iter().map(|(_, w)| *w).sum::<u32>()).max().unwrap_or(0);
            let changed = width != k.frame_width;
            k.frame_width = width;
            k.restart(self);
            changed
        });
        if remeasure == Some(true) {
            self.mark_layout_dirty();
        }
        self.request_render();
    }

    fn set_spinner_interval(self, interval: f64) {
        self.with_kind::<SpinnerKind, _>(|k| {
            if k.interval != interval {
                k.interval = interval;
                k.restart(self);
            }
        });
    }

    fn set_spinner_color(self, color: RGBA) {
        self.with_kind::<SpinnerKind, _>(|k| k.color = color);
        self.request_render();
    }
}

/// `<spinner frames={SPINNER_FRAMES} interval={80} color={color}>`.
pub fn spinner_element(color: Rc<dyn Fn() -> RGBA>) -> Element {
    let el = create_spinner();
    el.set_spinner_interval(80.0);
    bind_always(move || color(), move |c| el.set_spinner_color(c));
    el
}

/// The props of `<Spinner>`.
#[derive(Clone, Default)]
pub struct SpinnerProps {
    pub children: Option<Rc<dyn Fn() -> View>>,
    pub color: Option<Rc<dyn Fn() -> Option<RGBA>>>,
    pub shimmer: Option<Rc<dyn Fn() -> Option<RGBA>>>,
}

/// `<Spinner color shimmer>{children}</Spinner>`.
pub fn spinner(props: SpinnerProps) -> View {
    let t = tui();
    let theme = t.theme;
    let config = t.config_context.clone();
    let color: Rc<dyn Fn() -> RGBA> = {
        let color = props.color.clone();
        Rc::new(move || color.as_ref().and_then(|c| c()).unwrap_or_else(|| rgba(theme.with(|th| th.text.muted))))
    };
    let children = props.children.clone();
    let fallback_children = children.clone();
    let fallback_color = color.clone();
    show(
        move || config.data().animations.unwrap_or(true),
        move || {
            // Shimmer is drawn as the plain spinner (see the module doc).
            // A bare spinner beside a long sibling shrinks to a fractional width that rounds to 0, so the glyph
            // paints on top of the row gap. Only a labeled spinner may shrink (to wrap its text).
            let row = box_();
            row.set_flex_direction(FlexDirection::Row);
            row.set_gap(2, Some(Value::Point(1.0)));
            row.set_flex_shrink(Some(if children.is_some() { 1.0 } else { 0.0 }));
            let holder = box_();
            insert_node(row, holder, None);
            holder.set_flex_shrink(Some(0.0));
            insert_node(holder, spinner_element(color.clone()), None);
            if let Some(children) = children.clone() {
                let label = text();
                let c = color.clone();
                bind_always(move || c(), move |c| label.set_fg(Some(c)));
                insert(label, View::dynamic(move || children()), At::Only);
                insert_node(row, label, None);
            }
            View::Node(row)
        },
        Some(Rc::new(move || {
            // `<text fg={color()}>{props.children ? <>⋯ {props.children}</> : "⋯"}</text>`
            let el = text();
            let c = fallback_color.clone();
            bind_always(move || c(), move |c| el.set_fg(Some(c)));
            match fallback_children.clone() {
                Some(children) => {
                    insert_node(el, text_node_from_string("⋯ "), None);
                    insert(el, View::dynamic(move || children()), At::End);
                }
                None => insert(el, View::from("⋯"), At::Only),
            }
            View::Node(el)
        })),
    )
}
