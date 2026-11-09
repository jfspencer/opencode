//! Scene cases (`"k": "scene"`, harness/scene-exec.ts): a `TextRenderable` over [`SceneText`] as the scene drives it.
//! The node is absolutely positioned with a point width and a point or auto height, so its layout is its style,
//! with Yoga's measure for an auto height; every frame prepares the view at that layout (`prepareView`).

use crate::json::Value;
use crate::{behavior, color, draw, error, line_info, spans, wrap_mode};
use opencode_tui_text::scene_text::{SceneText, SceneTextOptions, SelectionOperation};
use opencode_tui_text::{StyledChunk, TextAlign, TextContext, TextError, WidthMethod};
use std::fmt::Write as _;

const WHITE: [u16; 4] = [255, 255, 255, 255];

struct Node {
    text: SceneText,
    options: SceneTextOptions,
    left: f64,
    top: f64,
    width: f32,
    height: Option<f32>,
    /// The layout of the last frame.
    layout: (f64, f64, f32, f32),
}

impl Node {
    fn style_size(&self) -> [Option<f32>; 2] {
        [Some(self.width), self.height]
    }

    /// `setNativeSceneTextOptions`.
    fn commit(&mut self) -> Result<(), TextError> {
        let size = self.style_size();
        self.text.set_options(self.options, || size).map(|_| ())
    }

    fn set_content(&mut self, v: &Value) -> Result<(), TextError> {
        if let Value::Str(s) = v {
            return self.text.set_text(s).map(|_| ());
        }
        let chunks: Vec<StyledChunk> = v
            .arr()
            .iter()
            .map(|c| StyledChunk {
                text: c.get("text").unwrap().str(),
                fg: color(c.get("fg")),
                bg: color(c.get("bg")),
                attributes: c.get("attributes").map_or(0, |a| a.u32()),
                link: c.get("link").map(|l| l.str()),
            })
            .collect();
        self.text.set_styled_text(&chunks).map(|_| ())
    }

    /// A frame: Yoga's layout (the measure for an auto height), then the view prepared at it.
    fn frame(&mut self) {
        let height = match self.height {
            Some(h) => h,
            None => self.text.yoga_measure(self.width, 1, f32::NAN, true).1,
        };
        self.layout = (self.left, self.top, self.width, height);
        if self.width > 0.0 && height > 0.0 {
            self.text.apply_layout(self.width as u32, height as u32);
        }
    }

    /// The `scrollX` and `scrollY` setters: clamped to the content past the layout size.
    fn scroll(&mut self, key: &str, value: f64) -> Result<(), TextError> {
        let metrics = self.text.metrics();
        let (current, max) = if key == "scrollX" {
            (self.options.scroll_x, (metrics.width_cols_max as f64 - self.layout.2 as f64).max(0.0))
        } else {
            (self.options.scroll_y, (metrics.virtual_line_count as f64 - self.layout.3 as f64).max(0.0))
        };
        let clamped = value.min(max).max(0.0);
        if current != clamped {
            let mut options = self.options;
            if key == "scrollX" {
                options.scroll_x = clamped;
            } else {
                options.scroll_y = clamped;
            }
            let size = self.style_size();
            self.text.set_options(options, || size)?;
            self.options = options;
        }
        Ok(())
    }

    /// The renderable's setters.
    fn set(&mut self, key: &str, v: &Value) -> Result<(), TextError> {
        let o = &mut self.options;
        match key {
            "content" => return self.set_content(v),
            "left" => self.left = v.num(),
            "top" => self.top = v.num(),
            "width" => self.width = v.num() as f32,
            "height" => self.height = (!v.is_null()).then(|| v.num() as f32),
            "fg" => o.fg = color(Some(v)).unwrap_or(WHITE),
            "bg" => o.bg = color(Some(v)).unwrap_or([0, 0, 0, 0]),
            "attributes" => o.attributes = if v.is_null() { 0 } else { v.u32() },
            "wrapMode" => {
                let mode = wrap_mode(v.str());
                if mode != o.wrap_mode {
                    o.wrap_mode = mode;
                    self.commit()?;
                    if v.str() != "none" {
                        self.scroll("scrollX", 0.0)?;
                    }
                }
                return Ok(());
            }
            "textAlign" => o.text_align = text_align(v.str()),
            "truncate" => {
                if o.truncate != v.bool() {
                    o.truncate = v.bool();
                    self.commit()?;
                    if v.bool() {
                        self.scroll("scrollX", 0.0)?;
                    }
                }
                return Ok(());
            }
            "tabIndicator" => o.tab_indicator = indicator(v),
            "tabIndicatorColor" => o.tab_indicator_color = color(Some(v)),
            _ => panic!("unknown property {key}"),
        }
        if !matches!(key, "left" | "top" | "width" | "height") {
            self.commit()?;
        }
        Ok(())
    }
}

fn text_align(s: &str) -> TextAlign {
    match s {
        "center" => TextAlign::Center,
        "right" => TextAlign::Right,
        _ => TextAlign::Left,
    }
}

fn indicator(v: &Value) -> u32 {
    match v {
        Value::Null => 0,
        Value::Str(s) => s.chars().next().map_or(0, |c| c as u32),
        _ => v.u32(),
    }
}

fn num(out: &mut String, v: f64) {
    if v.fract() == 0.0 {
        write!(out, "{}", v as i64).unwrap();
    } else {
        write!(out, "{v}").unwrap();
    }
}

pub fn run_scene_case(case: &Value, out: &mut String) {
    let method = WidthMethod::parse(case.get("m").unwrap().str()).unwrap();
    let (sw, sh) = (case.get("sw").unwrap().u32(), case.get("sh").unwrap().u32());
    let ctx = TextContext::new();
    let p = case.get("props").unwrap();
    let get = |k: &str| p.get(k).filter(|v| !v.is_null());
    let options = SceneTextOptions {
        fg: color(get("fg")).unwrap_or(WHITE),
        bg: color(get("bg")).unwrap_or([0, 0, 0, 0]),
        attributes: get("attributes").map_or(0, |v| v.u32()),
        wrap_mode: wrap_mode(get("wrapMode").map_or("word", |v| v.str())),
        text_align: text_align(get("textAlign").map_or("left", |v| v.str())),
        truncate: get("truncate").is_some_and(|v| v.bool()),
        scroll_x: 0.0,
        scroll_y: 0.0,
        first_line_offset: 0,
        tab_indicator: get("tabIndicator").map_or(0, indicator),
        tab_indicator_color: color(get("tabIndicatorColor")),
    };
    let mut node = Node {
        text: SceneText::new(method, &ctx),
        options,
        left: get("left").map_or(0.0, |v| v.num()),
        top: get("top").map_or(0.0, |v| v.num()),
        width: get("width").unwrap().num() as f32,
        height: get("height").map(|v| v.num() as f32),
        layout: (0.0, 0.0, 0.0, 0.0),
    };
    out.push('[');
    // The constructor commits the options, then the content (`""` without one).
    let mut created = node.commit();
    if created.is_ok() {
        created = match get("content") {
            Some(c) => node.set_content(c),
            None => node.text.set_text("").map(|_| ()),
        };
    }
    if let Err(e) = created {
        panic!("construction failed: {e:?}");
    }
    for (i, op) in case.get("ops").unwrap().arr().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        scene_op(&mut node, op.arr(), sw, sh, out);
    }
    out.push(']');
}

fn scene_op(node: &mut Node, op: &[Value], sw: u32, sh: u32, out: &mut String) {
    let unit = |out: &mut String, r: Result<(), TextError>| match r {
        Ok(()) => out.push_str("null"),
        Err(e) => error(out, e),
    };
    match op[0].str() {
        "sRender" => {
            node.frame();
            out.push_str("null");
        }
        "sDraw" => {
            let (x, y) = (op[1].i32(), op[2].i32());
            let mut grid = draw::Grid::new(sw, sh, [0, 0, 0, 255]);
            // `drawSceneBodyOn`: nothing for an origin outside the target or a node Yoga sized to zero.
            let (_, _, w, h) = node.layout;
            if x < sw as i32 && y < sh as i32 && w != 0.0 && h != 0.0 {
                node.text.draw(y, |src| draw::draw(src, &mut grid, x, y));
            }
            spans(out, &grid);
        }
        "sSet" => {
            let r = node.set(op[1].str(), &op[2]);
            unit(out, r)
        }
        "sScroll" => {
            let r = node.scroll(op[1].str(), op[2].num());
            match r {
                Ok(()) => {
                    out.push('[');
                    num(out, node.options.scroll_x);
                    out.push(',');
                    num(out, node.options.scroll_y);
                    out.push(']');
                }
                Err(e) => error(out, e),
            }
        }
        "sSel" => {
            let operation = match op[1].str() {
                "reset" => SelectionOperation::Reset,
                "set" => SelectionOperation::Set,
                _ => SelectionOperation::Update,
            };
            let b = op.get(6).filter(|v| !v.is_null()).map_or("cell", |v| v.str());
            let r = node.text.set_selection(
                operation,
                behavior(b),
                op[2].i32(),
                op[3].i32(),
                op[4].i32(),
                op[5].i32(),
                color(op.get(7)),
                color(op.get(8)),
            );
            match r {
                Ok(changed) => write!(out, "{changed}").unwrap(),
                Err(e) => error(out, e),
            }
        }
        "sSelection" => match node.text.selection() {
            Some((start, end)) => write!(out, "[{start},{end}]").unwrap(),
            None => out.push_str("null"),
        },
        "sSelected" => crate::json::write_str(out, &node.text.selected_text()),
        "sInfo" => {
            let m = node.text.metrics();
            write!(out, "[{},{},{},{},{},", m.line_count, m.virtual_line_count, m.width_cols_max, m.virtual_line_count, m.text_length)
                .unwrap();
            crate::json::write_str(out, &node.text.text());
            out.push(']');
        }
        "sLayout" => {
            let (x, y, w, h) = node.layout;
            out.push('[');
            num(out, x);
            out.push(',');
            num(out, y);
            out.push(',');
            num(out, w as f64);
            out.push(',');
            num(out, h as f64);
            out.push(']');
        }
        "sLineInfo" => line_info(out, &node.text.line_info(0, u32::MAX)),
        "sLineSources" => {
            let info = node.text.line_info(op[1].u32(), op[2].u32());
            out.push('[');
            for (i, s) in info.line_sources.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write!(out, "{s}").unwrap();
            }
            out.push(']');
        }
        name => panic!("unknown op {name}"),
    }
}
