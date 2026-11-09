//! `packages/core/src/lib/yoga.options.ts`: the string forms Renderable options take (`"row"`, `"space-between"`)
//! and their Yoga enums. Matching is on `toLowerCase()`; anything unknown, and a missing value (`None`), gives the
//! same default TS gives.
use crate::{Align, BoxSizing, Dimension, Direction, Display, Edge, FlexDirection, Gutter, Justify, LogLevel};
use crate::{MeasureMode, Overflow, PositionType, Unit, Wrap};

fn lower(value: Option<&str>) -> Option<String> {
    value.map(str::to_lowercase)
}

pub fn parse_align(value: Option<&str>) -> Align {
    match lower(value).as_deref() {
        Some("flex-start") => Align::FlexStart,
        Some("center") => Align::Center,
        Some("flex-end") => Align::FlexEnd,
        Some("stretch") => Align::Stretch,
        Some("baseline") => Align::Baseline,
        Some("space-between") => Align::SpaceBetween,
        Some("space-around") => Align::SpaceAround,
        Some("space-evenly") => Align::SpaceEvenly,
        _ => Align::Auto,
    }
}

pub fn parse_align_items(value: Option<&str>) -> Align {
    match lower(value).as_deref() {
        Some("auto") => Align::Auto,
        Some("flex-start") => Align::FlexStart,
        Some("center") => Align::Center,
        Some("flex-end") => Align::FlexEnd,
        Some("baseline") => Align::Baseline,
        Some("space-between") => Align::SpaceBetween,
        Some("space-around") => Align::SpaceAround,
        Some("space-evenly") => Align::SpaceEvenly,
        _ => Align::Stretch,
    }
}

pub fn parse_box_sizing(value: Option<&str>) -> BoxSizing {
    match lower(value).as_deref() {
        Some("content-box") => BoxSizing::ContentBox,
        _ => BoxSizing::BorderBox,
    }
}

pub fn parse_dimension(value: Option<&str>) -> Dimension {
    match lower(value).as_deref() {
        Some("height") => Dimension::Height,
        _ => Dimension::Width,
    }
}

pub fn parse_direction(value: Option<&str>) -> Direction {
    match lower(value).as_deref() {
        Some("inherit") => Direction::Inherit,
        Some("rtl") => Direction::RTL,
        _ => Direction::LTR,
    }
}

pub fn parse_display(value: Option<&str>) -> Display {
    match lower(value).as_deref() {
        Some("none") => Display::None,
        Some("contents") => Display::Contents,
        _ => Display::Flex,
    }
}

pub fn parse_edge(value: Option<&str>) -> Edge {
    match lower(value).as_deref() {
        Some("left") => Edge::Left,
        Some("top") => Edge::Top,
        Some("right") => Edge::Right,
        Some("bottom") => Edge::Bottom,
        Some("start") => Edge::Start,
        Some("end") => Edge::End,
        Some("horizontal") => Edge::Horizontal,
        Some("vertical") => Edge::Vertical,
        _ => Edge::All,
    }
}

pub fn parse_flex_direction(value: Option<&str>) -> FlexDirection {
    match lower(value).as_deref() {
        Some("column-reverse") => FlexDirection::ColumnReverse,
        Some("row") => FlexDirection::Row,
        Some("row-reverse") => FlexDirection::RowReverse,
        _ => FlexDirection::Column,
    }
}

pub fn parse_gutter(value: Option<&str>) -> Gutter {
    match lower(value).as_deref() {
        Some("column") => Gutter::Column,
        Some("row") => Gutter::Row,
        _ => Gutter::All,
    }
}

pub fn parse_justify(value: Option<&str>) -> Justify {
    match lower(value).as_deref() {
        Some("center") => Justify::Center,
        Some("flex-end") => Justify::FlexEnd,
        Some("space-between") => Justify::SpaceBetween,
        Some("space-around") => Justify::SpaceAround,
        Some("space-evenly") => Justify::SpaceEvenly,
        _ => Justify::FlexStart,
    }
}

pub fn parse_log_level(value: Option<&str>) -> LogLevel {
    match lower(value).as_deref() {
        Some("error") => LogLevel::Error,
        Some("warn") => LogLevel::Warn,
        Some("debug") => LogLevel::Debug,
        Some("verbose") => LogLevel::Verbose,
        Some("fatal") => LogLevel::Fatal,
        _ => LogLevel::Info,
    }
}

pub fn parse_measure_mode(value: Option<&str>) -> MeasureMode {
    match lower(value).as_deref() {
        Some("exactly") => MeasureMode::Exactly,
        Some("at-most") => MeasureMode::AtMost,
        _ => MeasureMode::Undefined,
    }
}

pub fn parse_overflow(value: Option<&str>) -> Overflow {
    match lower(value).as_deref() {
        Some("hidden") => Overflow::Hidden,
        Some("scroll") => Overflow::Scroll,
        _ => Overflow::Visible,
    }
}

/// A missing value is `Relative`, an unknown one `Static`, as in TS.
pub fn parse_position_type(value: Option<&str>) -> PositionType {
    match lower(value).as_deref() {
        None => PositionType::Relative,
        Some("relative") => PositionType::Relative,
        Some("absolute") => PositionType::Absolute,
        Some(_) => PositionType::Static,
    }
}

pub fn parse_unit(value: Option<&str>) -> Unit {
    match lower(value).as_deref() {
        Some("undefined") => Unit::Undefined,
        Some("percent") => Unit::Percent,
        Some("auto") => Unit::Auto,
        _ => Unit::Point,
    }
}

pub fn parse_wrap(value: Option<&str>) -> Wrap {
    match lower(value).as_deref() {
        Some("wrap") => Wrap::Wrap,
        Some("wrap-reverse") => Wrap::WrapReverse,
        _ => Wrap::NoWrap,
    }
}
