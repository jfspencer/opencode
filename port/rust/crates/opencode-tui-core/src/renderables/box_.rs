//! `<box>`: `Box.ts` (background, borders, border style and colors, focused border color, titles, shouldFill,
//! gaps) painted as the scene's `paintBox` does (`drawBox` when it has a border or a filled, non-transparent
//! background; the focused border color while focusable and focused).
use std::any::Any;

use crate::border::{BorderSides, BorderStyle, TitleAlignment};
use crate::buffer::{DrawBoxOptions, OptimizedBuffer};
use crate::color::{RGBA, parse_color};
use crate::tree::{self, Element, Kind};

/// The `border` prop: `true`/`false` or a list of sides.
#[derive(Clone, Debug, PartialEq)]
pub enum BorderProp {
    All(bool),
    Sides(Vec<Side>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl BorderProp {
    fn sides(&self) -> BorderSides {
        match self {
            BorderProp::All(on) => BorderSides { top: *on, right: *on, bottom: *on, left: *on },
            BorderProp::Sides(sides) => BorderSides {
                top: sides.contains(&Side::Top),
                right: sides.contains(&Side::Right),
                bottom: sides.contains(&Side::Bottom),
                left: sides.contains(&Side::Left),
            },
        }
    }
}

fn mask(sides: BorderSides) -> u32 {
    (sides.left as u32) | (sides.top as u32) << 1 | (sides.right as u32) << 2 | (sides.bottom as u32) << 3
}

pub struct BoxKind {
    pub background: RGBA,
    pub border: BorderProp,
    pub sides: BorderSides,
    pub border_style: BorderStyle,
    pub border_color: RGBA,
    pub focused_border_color: RGBA,
    pub custom_border_chars: Option<[u32; 11]>,
    pub should_fill: bool,
    pub title: Option<String>,
    pub title_color: Option<RGBA>,
    pub title_alignment: TitleAlignment,
    pub bottom_title: Option<String>,
    pub bottom_title_alignment: TitleAlignment,
}

impl Default for BoxKind {
    fn default() -> Self {
        BoxKind {
            background: parse_color("transparent"),
            border: BorderProp::All(false),
            sides: BorderProp::All(false).sides(),
            border_style: BorderStyle::Single,
            border_color: parse_color("#FFFFFF"),
            focused_border_color: parse_color("#00AAFF"),
            custom_border_chars: None,
            should_fill: true,
            title: None,
            title_color: None,
            title_alignment: TitleAlignment::Left,
            bottom_title: None,
            bottom_title_alignment: TitleAlignment::Left,
        }
    }
}

impl BoxKind {
    fn has_paint(&self) -> bool {
        mask(self.sides) != 0 || (self.should_fill && self.background.a() > 0.0)
    }
}

impl Kind for BoxKind {
    fn name(&self) -> &'static str {
        "box"
    }
    fn paints(&self) -> bool {
        self.has_paint()
    }
    fn render(&mut self, el: Element, buffer: &mut OptimizedBuffer, x: i32, y: i32, width: u32, height: u32) {
        if !self.has_paint() {
            return;
        }
        let focused = el.with_base(|b| b.focusable && b.focused).unwrap_or(false);
        let border_color = if focused { self.focused_border_color } else { self.border_color };
        buffer.draw_box(&DrawBoxOptions {
            x,
            y,
            width: width as i32,
            height: height as i32,
            border_style: self.border_style,
            custom_border_chars: self.custom_border_chars,
            border: self.sides,
            border_color,
            background_color: self.background,
            should_fill: self.should_fill,
            title: self.title.as_deref(),
            title_color: self.title_color,
            title_alignment: self.title_alignment,
            bottom_title: self.bottom_title.as_deref(),
            bottom_title_alignment: self.bottom_title_alignment,
        });
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Creates a `<box>`.
pub fn create_box() -> Element {
    tree::create(Box::new(BoxKind::default()), "box", false)
}

impl Element {
    /// The box of a `<box>`, or the box a `<scrollbox>` extends.
    fn with_box<R>(self, f: impl FnOnce(&mut BoxKind) -> R) -> Option<R> {
        let r = if self.is::<crate::renderables::scrollbox::ScrollBoxKind>() {
            self.with_kind::<crate::renderables::scrollbox::ScrollBoxKind, _>(|k| f(&mut k.boxed))
        } else {
            self.with_kind::<BoxKind, _>(f)
        };
        self.request_render();
        r
    }

    fn sync_border(self) {
        let sides = if self.is::<crate::renderables::scrollbox::ScrollBoxKind>() {
            self.with_kind::<crate::renderables::scrollbox::ScrollBoxKind, _>(|k| mask(k.boxed.sides))
        } else {
            self.with_kind::<BoxKind, _>(|k| mask(k.sides))
        };
        if let Some(m) = sides {
            if self.with_base(|b| b.border_mask) != Some(m) {
                self.set_border_mask(m);
            }
        }
    }

    /// `initializeBorder`: setting a border style or color turns a `false` border on.
    fn initialize_border(k: &mut BoxKind) {
        if k.border == BorderProp::All(false) {
            k.border = BorderProp::All(true);
            k.sides = k.border.sides();
        }
    }

    pub fn set_background_color(self, color: Option<RGBA>) {
        self.with_box(|k| k.background = color.unwrap_or(parse_color("transparent")));
    }

    pub fn set_border(self, border: BorderProp) {
        self.with_box(|k| {
            k.sides = border.sides();
            k.border = border;
        });
        self.sync_border();
    }

    pub fn set_border_style(self, style: Option<BorderStyle>) {
        self.with_box(|k| {
            k.border_style = style.unwrap_or(BorderStyle::Single);
            k.custom_border_chars = None;
            Self::initialize_border(k);
        });
        self.sync_border();
    }

    pub fn set_border_color(self, color: Option<RGBA>) {
        self.with_box(|k| {
            k.border_color = color.unwrap_or(parse_color("#FFFFFF"));
            Self::initialize_border(k);
        });
        self.sync_border();
    }

    pub fn set_focused_border_color(self, color: Option<RGBA>) {
        self.with_box(|k| {
            k.focused_border_color = color.unwrap_or(parse_color("#00AAFF"));
            Self::initialize_border(k);
        });
        self.sync_border();
    }

    pub fn set_custom_border_chars(self, chars: Option<[u32; 11]>) {
        self.with_box(|k| {
            k.custom_border_chars = chars;
            if chars.is_some() {
                Self::initialize_border(k);
            }
        });
        self.sync_border();
    }

    pub fn set_should_fill(self, fill: Option<bool>) {
        self.with_box(|k| k.should_fill = fill.unwrap_or(true));
    }

    pub fn set_title(self, title: Option<String>) {
        self.with_box(|k| k.title = title);
    }

    pub fn set_title_color(self, color: Option<RGBA>) {
        self.with_box(|k| k.title_color = color);
    }

    pub fn set_title_alignment(self, alignment: TitleAlignment) {
        self.with_box(|k| k.title_alignment = alignment);
    }

    pub fn set_bottom_title(self, title: Option<String>) {
        self.with_box(|k| k.bottom_title = title);
    }

    pub fn set_bottom_title_alignment(self, alignment: TitleAlignment) {
        self.with_box(|k| k.bottom_title_alignment = alignment);
    }
}
