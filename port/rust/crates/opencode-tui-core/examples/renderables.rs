//! The Rust side of the renderables harness (harness/renderables.ts): builds each case's scene (LineNumber, Diff)
//! on the tree, renders a few frames without a renderer, and prints every cell as renderables.ts prints the TS
//! test renderer's frame.
//!
//! usage: renderables <cases file>
use std::collections::{HashMap, HashSet};

use opencode_protocol::Json;
use opencode_tui_core::buffer::OptimizedBuffer;
use opencode_tui_core::color::{RGBA, parse_color};
use opencode_tui_core::elements::{box_, text};
use opencode_tui_core::layout::Value;
use opencode_tui_core::pool;
use opencode_tui_core::renderables::line_number::{
    GutterLayout, LineColor, LineColorConfig, LineNumberOptions, LineSign, create_line_number,
};
use opencode_tui_core::renderables::text::WrapMode;
use opencode_tui_core::renderer;
use opencode_tui_core::tree::{self, FlexDirection};
use opencode_tui_core::unicode::WidthMethod;

fn get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    match value {
        Json::Object(o) => o.get(key).filter(|v| !matches!(v, Json::Null)),
        _ => None,
    }
}
fn str_of<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    match get(value, key) {
        Some(Json::String(s)) => Some(s),
        _ => None,
    }
}
fn num(value: &Json, key: &str) -> Option<f64> {
    match get(value, key) {
        Some(Json::Number(n)) => Some(*n),
        _ => None,
    }
}
fn arr<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    match get(value, key) {
        Some(Json::Array(items)) => items,
        _ => &[],
    }
}
fn color(value: Option<&Json>) -> Option<RGBA> {
    match value {
        Some(Json::String(s)) => Some(parse_color(s.as_str())),
        _ => None,
    }
}
fn wrap(value: &str) -> WrapMode {
    match value {
        "char" => WrapMode::Char,
        "none" => WrapMode::None,
        _ => WrapMode::Word,
    }
}

fn hex2(value: f64) -> String {
    format!("{:02x}", (value.clamp(0.0, 1.0) * 255.0).round() as u32)
}
fn encode(c: RGBA) -> String {
    format!("{}{}{}{}", hex2(c.r()), hex2(c.g()), hex2(c.b()), hex2(c.a()))
}

fn dump(name: &str, buffer: &OptimizedBuffer) -> Vec<String> {
    let mut out = vec![format!("dump {name}")];
    for y in 0..buffer.height() {
        let mut chars = String::new();
        let mut cells = Vec::new();
        for x in 0..buffer.width() {
            let cell = buffer.get(x, y).unwrap();
            if pool::is_grapheme_char(cell.char) {
                let bytes = pool::grapheme_bytes(pool::grapheme_id_from_char(cell.char)).unwrap_or_default();
                chars.push_str(&String::from_utf8_lossy(&bytes));
            } else if !pool::is_continuation_char(cell.char) {
                chars.push(char::from_u32(cell.char).unwrap_or(' '));
            }
            cells.push(format!("{}/{}", encode(cell.fg), encode(cell.bg)));
        }
        out.push(format!("{y} {chars}"));
        out.push(format!("{y} {}", cells.join(" ")));
    }
    out
}

fn line_color(value: &Json) -> LineColor {
    match value {
        Json::String(s) => LineColor::Color(parse_color(s.as_str())),
        other => LineColor::Config(LineColorConfig { gutter: color(get(other, "gutter")), content: color(get(other, "content")) }),
    }
}

/// `new Map(pairs)`: a repeated key keeps its first position and takes the last value.
fn map_like<V>(pairs: Vec<(i64, V)>) -> Vec<(i64, V)> {
    let mut out: Vec<(i64, V)> = Vec::new();
    for (key, value) in pairs {
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => out.push((key, value)),
        }
    }
    out
}

fn line_number_case(case: &Json, container: tree::Element) {
    let ln = create_line_number(LineNumberOptions::default());
    ln.set_id("ln");
    if let Some(fg) = color(get(case, "fg")) {
        ln.set_line_number_fg(Some(fg));
    }
    if let Some(bg) = color(get(case, "bg")) {
        ln.set_line_number_bg(Some(bg));
    }
    if let Some(min_width) = num(case, "minWidth") {
        ln.set_min_width(Some(Value::Point(min_width as f32)));
    }
    if let Some(padding) = num(case, "paddingRight") {
        ln.set_padding(tree::Edge::Right, Some(Value::Point(padding as f32)));
    }
    let target = text();
    target.set_id("t");
    target.set_wrap_mode(wrap(str_of(case, "wrap").unwrap_or("word")));
    target.set_content(str_of(case, "content").unwrap_or(""));
    ln.add(target, None);
    if get(case, "lineColors").is_some() {
        let colors: Vec<(i64, LineColor)> = arr(case, "lineColors")
            .iter()
            .map(|pair| match pair {
                Json::Array(p) => (match p[0] {
                    Json::Number(n) => n as i64,
                    _ => 0,
                }, line_color(&p[1])),
                _ => unreachable!(),
            })
            .collect();
        ln.set_line_colors(&map_like(colors));
    }
    if get(case, "signs").is_some() {
        let signs = arr(case, "signs")
            .iter()
            .map(|pair| match pair {
                Json::Array(p) => {
                    let line = match p[0] {
                        Json::Number(n) => n as i64,
                        _ => 0,
                    };
                    let s = &p[1];
                    (
                        line,
                        LineSign {
                            before: str_of(s, "before").map(str::to_string),
                            before_color: color(get(s, "beforeColor")),
                            after: str_of(s, "after").map(str::to_string),
                            after_color: color(get(s, "afterColor")),
                        },
                    )
                }
                _ => unreachable!(),
            })
            .collect();
        ln.set_line_signs(map_like(signs));
    }
    if get(case, "hide").is_some() {
        let hide: HashSet<i64> = arr(case, "hide")
            .iter()
            .filter_map(|v| match v {
                Json::Number(n) => Some(*n as i64),
                _ => None,
            })
            .collect();
        ln.set_hide_line_numbers(hide);
    }
    if get(case, "numbers").is_some() {
        let numbers: HashMap<i64, f64> = arr(case, "numbers")
            .iter()
            .filter_map(|pair| match pair {
                Json::Array(p) => match (&p[0], &p[1]) {
                    (Json::Number(l), Json::Number(n)) => Some((*l as i64, *n)),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        ln.set_line_numbers(numbers);
    }
    if let Some(offset) = num(case, "offset") {
        ln.set_line_number_offset(offset as i64);
    }
    if let Some(layout) = get(case, "layout") {
        let n = |key| num(layout, key).unwrap_or(0.0) as u32;
        ln.set_gutter_layout(GutterLayout {
            min_width: n("minWidth"),
            padding_left: n("paddingLeft"),
            padding_right: n("paddingRight"),
            min_digits: n("minDigits"),
            min_after_width: n("minAfterWidth"),
        });
    }
    if matches!(get(case, "show"), Some(Json::Bool(false))) {
        ln.set_show_line_numbers(false);
    }
    container.add(ln, None);
}

fn frames(width: u32, height: u32, count: usize) -> OptimizedBuffer {
    let mut buffer = OptimizedBuffer::new(width, height, false, WidthMethod::Unicode);
    for _ in 0..count {
        tree::prepare_frame_for_tests(width, height);
        buffer.set_blend_backdrop_color(Some(RGBA::rgb(0, 0, 0, 255)));
        buffer.clear(RGBA::rgb(0, 0, 0, 0));
        tree::paint_for_tests(&mut buffer);
        renderer::run_ticks_for_tests();
    }
    buffer
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: renderables <cases file>");
    let cases = std::fs::read_to_string(path).unwrap();
    let mut out = Vec::new();
    for line in cases.lines().filter(|l| !l.is_empty()) {
        let case = opencode_protocol::wire::json::parse(line).unwrap();
        let name = str_of(&case, "name").unwrap_or("").to_string();
        let width = num(&case, "width").unwrap_or(40.0) as u32;
        let height = num(&case, "height").unwrap_or(10.0) as u32;
        let root = tree::create_root_for_tests(width, height);
        let container = box_();
        container.set_id("box");
        container.set_width(Some(Value::Point(num(&case, "boxWidth").unwrap_or(40.0) as f32)));
        container.set_flex_direction(FlexDirection::Column);
        root.add(container, None);
        match str_of(&case, "kind") {
            Some("line_number") => line_number_case(&case, container),
            Some("diff") => {
                if !opencode_tui_core_diff::run(&case, container) {
                    out.push(format!("error {name} unsupported"));
                    root.destroy_recursively();
                    continue;
                }
            }
            _ => {}
        }
        let mut buffer = frames(width, height, 3);
        if opencode_tui_core_diff::later() {
            buffer = frames(width, height, 3);
        }
        out.extend(dump(&name, &buffer));
        root.destroy_recursively();
    }
    println!("{}", out.join("\n"));
}

/// Diff cases: as @opentui/solid creates a `<diff>` (the id, then each prop).
mod opencode_tui_core_diff {
    use super::*;
    use opencode_tui_core::renderables::diff::{DiffGutterOptions, DiffView, create_diff};

    fn view(value: &str) -> DiffView {
        if value == "split" { DiffView::Split } else { DiffView::Unified }
    }

    pub fn run(case: &Json, container: tree::Element) -> bool {
        let diff = create_diff();
        diff.set_id("diff");
        if let Some(v) = str_of(case, "view") {
            diff.set_diff_view(view(v));
        }
        diff.set_diff_show_line_numbers(!matches!(get(case, "showLineNumbers"), Some(Json::Bool(false))));
        diff.set_width(Some(Value::Percent(100.0)));
        if let Some(w) = str_of(case, "wrap") {
            diff.set_diff_wrap_mode(Some(wrap(w)));
        }
        if let Some(fg) = color(get(case, "fg")) {
            diff.set_diff_fg(Some(fg));
        }
        if let Some(Json::Object(colors)) = get(case, "colors") {
            for (key, value) in colors.iter() {
                let c = color(Some(value));
                match key.as_str() {
                    "addedBg" => diff.set_diff_added_bg(c),
                    "removedBg" => diff.set_diff_removed_bg(c),
                    "contextBg" => diff.set_diff_context_bg(c),
                    "addedSignColor" => diff.set_diff_added_sign_color(c),
                    "removedSignColor" => diff.set_diff_removed_sign_color(c),
                    "lineNumberFg" => diff.set_diff_line_number_fg(c),
                    "lineNumberBg" => diff.set_diff_line_number_bg(c),
                    "addedLineNumberBg" => diff.set_diff_added_line_number_bg(c),
                    "removedLineNumberBg" => diff.set_diff_removed_line_number_bg(c),
                    "addedContentBg" => diff.set_diff_added_content_bg(c),
                    other => panic!("color {other}"),
                }
            }
        }
        diff.set_diff(str_of(case, "diff").unwrap_or(""));
        if let Some(h) = num(case, "minHeight") {
            diff.set_min_height(Some(Value::Point(h as f32)));
        }
        if let Some(g) = get(case, "gutter") {
            diff.set_diff_gutter(DiffGutterOptions {
                padding_left: num(g, "paddingLeft").map(|v| v as u32),
                padding_right: num(g, "paddingRight").map(|v| v as u32),
                min_digits: num(g, "minDigits").map(|v| v as u32),
            });
        }
        container.add(diff, None);
        LATER.with(|l| *l.borrow_mut() = get(case, "later").cloned().map(|later| (diff, container, later)));
        true
    }

    thread_local! {
        static LATER: std::cell::RefCell<Option<(tree::Element, tree::Element, Json)>> = const { std::cell::RefCell::new(None) };
    }

    /// The prop changes applied after the first frames.
    pub fn later() -> bool {
        let Some((diff, container, later)) = LATER.with(|l| l.borrow_mut().take()) else { return false };
        if let Some(v) = str_of(&later, "view") {
            diff.set_diff_view(view(v));
        }
        if let Some(w) = str_of(&later, "wrap") {
            diff.set_diff_wrap_mode(Some(wrap(w)));
        }
        if let Some(d) = str_of(&later, "diff") {
            diff.set_diff(d);
        }
        if let Some(w) = num(&later, "boxWidth") {
            container.set_width(Some(Value::Point(w as f32)));
        }
        true
    }
}
