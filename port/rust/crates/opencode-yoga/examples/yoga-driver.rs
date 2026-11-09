//! The Rust side of harness/oracle.ts: replays its programs (one op per line, `end` after each) through
//! opencode-yoga and writes the answers in the oracle's format. The yoga.ts ops (`ins`, `ts`, `lay`, ...) go through
//! the typed methods, the native ones (`se`, `mv`, `layr`, `nmeas`, ...) through the yoga.zig-named ones.
//!
//! usage: yoga-driver <programs> <results>
use opencode_yoga::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

type Out = Rc<RefCell<Vec<String>>>;

fn hx(x: f32) -> String {
    if x.is_nan() { "nan".to_owned() } else { format!("{:08x}", x.to_bits()) }
}

fn number(token: &str) -> f64 {
    match token {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => token.parse().unwrap_or_else(|_| panic!("number {token}")),
    }
}

fn int(token: &str) -> u32 {
    token.parse().unwrap_or_else(|_| panic!("integer {token}"))
}

fn esc(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if (' '..='~').contains(&ch) && ch != '\\' {
            out.push(ch);
        } else {
            let _ = write!(out, "\\u{{{:x}}}", ch as u32);
        }
    }
    out
}

fn err_text(error: &Error) -> String {
    format!("err {}", esc(&error.to_string()))
}

fn decode_uri_component(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            out.push(u8::from_str_radix(&text[index + 1..index + 3], 16).unwrap());
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// JS `Math.min`/`Math.max` of two numbers: NaN wins, and -0 is less than +0.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() { a } else { b }
    } else if a < b {
        a
    } else {
        b
    }
}

fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() { b } else { a }
    } else if a > b {
        a
    } else {
        b
    }
}

/// oracle.ts `measureSpec`.
fn measure_spec(kind: &str, a: f64, b: f64) -> impl Fn(f64, u32, f64, u32) -> (f64, f64) + 'static {
    let kind = kind.to_owned();
    move |w, wm, h, hm| match kind.as_str() {
        "fixed" => (a, b),
        "text" => {
            let limit = if wm == 0 { a } else { js_max(1.0, w.floor()) };
            let width = js_min(a, limit);
            let mut height = if width > 0.0 { (a / width).ceil() } else { 0.0 };
            if hm == 2 {
                height = js_min(height, h);
            }
            (width, height)
        }
        "frac" => (if wm == 0 { a } else { w * 0.5 + a }, if hm == 0 { b } else { h * 0.25 + b }),
        "echo" => (if wm == 0 { a } else { w }, if hm == 0 { b } else { h }),
        _ => panic!("measure kind {kind}"),
    }
}

fn measure_function(out: &Out, id: usize, args: &[&str]) -> MeasureFunction {
    let spec = measure_spec(args[0], number(args[1]), number(args[2]));
    let out = out.clone();
    Box::new(move |w, wm, h, hm| {
        out.borrow_mut().push(format!("m {id} {} {} {} {}", hx(w), wm as u32, hx(h), hm as u32));
        let (width, height) = spec(w as f64, wm as u32, h as f64, hm as u32);
        Size { width: width as f32, height: height as f32 }
    })
}

fn value_input(token: &str) -> std::result::Result<ValueInput, Error> {
    Ok(match token {
        "u" => ValueInput::Undefined,
        "a" => ValueInput::Auto,
        _ if token.starts_with("n:") => ValueInput::Point(number(&token[2..])),
        _ if token.starts_with("s:") => ValueInput::from(decode_uri_component(&token[2..])),
        _ if token.starts_with("v:") => {
            let mut parts = token[2..].splitn(2, ':');
            let unit = Unit::from_raw(int(parts.next().unwrap())).unwrap();
            ValueInput::Value(Value { unit, value: number(parts.next().unwrap()) as f32 })
        }
        _ => panic!("value input {token}"),
    })
}

fn opt_number(token: &str) -> Option<f64> {
    (token != "u").then(|| number(token))
}

fn layout_input(token: &str) -> Option<f64> {
    (token != "u" && token != "a").then(|| number(token))
}

fn edge(token: &str) -> Edge {
    Edge::from_raw(int(token)).unwrap()
}

fn gutter(token: &str) -> Gutter {
    Gutter::from_raw(int(token)).unwrap()
}

fn ts_call(node: &Node, method: &str, args: &[&str]) -> Result<()> {
    let e = |i: usize| int(args[i]);
    let vi = |i: usize| value_input(args[i]);
    match method {
        "setDirection" => node.set_direction(Direction::from_raw(e(0)).unwrap()),
        "setFlexDirection" => node.set_flex_direction(FlexDirection::from_raw(e(0)).unwrap()),
        "setJustifyContent" => node.set_justify_content(Justify::from_raw(e(0)).unwrap()),
        "setAlignContent" => node.set_align_content(Align::from_raw(e(0)).unwrap()),
        "setAlignItems" => node.set_align_items(Align::from_raw(e(0)).unwrap()),
        "setAlignSelf" => node.set_align_self(Align::from_raw(e(0)).unwrap()),
        "setPositionType" => node.set_position_type(PositionType::from_raw(e(0)).unwrap()),
        "setFlexWrap" => node.set_flex_wrap(Wrap::from_raw(e(0)).unwrap()),
        "setOverflow" => node.set_overflow(Overflow::from_raw(e(0)).unwrap()),
        "setDisplay" => node.set_display(Display::from_raw(e(0)).unwrap()),
        "setBoxSizing" => node.set_box_sizing(BoxSizing::from_raw(e(0)).unwrap()),
        "setFlex" => node.set_flex(opt_number(args[0])),
        "setFlexGrow" => node.set_flex_grow(opt_number(args[0])),
        "setFlexShrink" => node.set_flex_shrink(opt_number(args[0])),
        "setAspectRatio" => node.set_aspect_ratio(opt_number(args[0])),
        "setFlexBasis" => node.set_flex_basis(vi(0)?),
        "setWidth" => node.set_width(vi(0)?),
        "setHeight" => node.set_height(vi(0)?),
        "setMinWidth" => node.set_min_width(vi(0)?),
        "setMinHeight" => node.set_min_height(vi(0)?),
        "setMaxWidth" => node.set_max_width(vi(0)?),
        "setMaxHeight" => node.set_max_height(vi(0)?),
        "setFlexBasisPercent" => node.set_flex_basis_percent(opt_number(args[0])),
        "setWidthPercent" => node.set_width_percent(opt_number(args[0])),
        "setHeightPercent" => node.set_height_percent(opt_number(args[0])),
        "setMinWidthPercent" => node.set_min_width_percent(opt_number(args[0])),
        "setMinHeightPercent" => node.set_min_height_percent(opt_number(args[0])),
        "setMaxWidthPercent" => node.set_max_width_percent(opt_number(args[0])),
        "setMaxHeightPercent" => node.set_max_height_percent(opt_number(args[0])),
        "setFlexBasisAuto" => node.set_flex_basis_auto(),
        "setWidthAuto" => node.set_width_auto(),
        "setHeightAuto" => node.set_height_auto(),
        "setMargin" => node.set_margin(edge(args[0]), vi(1)?),
        "setPadding" => node.set_padding(edge(args[0]), vi(1)?),
        "setPosition" => node.set_position(edge(args[0]), vi(1)?),
        "setGap" => node.set_gap(gutter(args[0]), vi(1)?),
        "setMarginPercent" => node.set_margin_percent(edge(args[0]), opt_number(args[1])),
        "setPaddingPercent" => node.set_padding_percent(edge(args[0]), opt_number(args[1])),
        "setPositionPercent" => node.set_position_percent(edge(args[0]), opt_number(args[1])),
        "setGapPercent" => node.set_gap_percent(gutter(args[0]), opt_number(args[1])),
        "setBorder" => node.set_border(edge(args[0]), opt_number(args[1])),
        "setMarginAuto" => node.set_margin_auto(edge(args[0])),
        "setPositionAuto" => node.set_position_auto(edge(args[0])),
        "setIsReferenceBaseline" => node.set_is_reference_baseline(args[0] == "1"),
        "setAlwaysFormsContainingBlock" => node.set_always_forms_containing_block(args[0] == "1"),
        _ => panic!("method {method}"),
    }
}

fn value(v: Value) -> String {
    format!("{}:{}", v.unit as u32, hx(v.value))
}

fn getters(node: &Node) -> String {
    let mut parts: Vec<String> = Vec::new();
    fn add(parts: &mut Vec<String>, result: Result<String>) {
        parts.push(result.unwrap_or_else(|error| err_text(&error)));
    }
    add(&mut parts, node.get_direction().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_flex_direction().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_justify_content().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_align_content().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_align_items().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_align_self().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_position_type().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_flex_wrap().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_overflow().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_display().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_box_sizing().map(|v| (v as u32).to_string()));
    for result in [node.get_flex(), node.get_flex_grow(), node.get_flex_shrink(), node.get_aspect_ratio()] {
        add(&mut parts, result.map(hx));
    }
    for result in [
        node.get_flex_basis(),
        node.get_width(),
        node.get_height(),
        node.get_min_width(),
        node.get_min_height(),
        node.get_max_width(),
        node.get_max_height(),
    ] {
        add(&mut parts, result.map(value));
    }
    for getter in [Node::get_margin, Node::get_padding, Node::get_position] {
        for e in 0..=8 {
            add(&mut parts, getter(node, Edge::from_raw(e).unwrap()).map(value));
        }
    }
    for g in 0..=2 {
        add(&mut parts, node.get_gap(Gutter::from_raw(g).unwrap()).map(value));
    }
    for e in 0..=8 {
        add(&mut parts, node.get_border(Edge::from_raw(e).unwrap()).map(hx));
    }
    add(&mut parts, node.is_reference_baseline().map(|v| (v as u32).to_string()));
    add(&mut parts, node.get_always_forms_containing_block().map(|v| (v as u32).to_string()));
    parts.push((node.has_measure_func() as u32).to_string());
    parts.push(node.get_child_count().to_string());
    parts.join(" ")
}

struct State {
    configs: Vec<Option<Config>>,
    nodes: Vec<Option<Node>>,
    ids: HashMap<Node, usize>,
    out: Out,
}

impl State {
    fn id(&self, node: Option<Node>) -> String {
        node.map_or("null".to_owned(), |node| self.ids[&node].to_string())
    }

    fn dump(&self, node: &Node) {
        let mut parts = vec![format!("n {}", self.ids[node])];
        let mut row = || -> Result<()> {
            let l = node.get_computed_layout()?;
            parts.push([l.left, l.top, l.right, l.bottom, l.width, l.height].map(hx).join(" "));
            for getter in [Node::get_computed_margin, Node::get_computed_padding, Node::get_computed_border] {
                let mut edges = Vec::new();
                for e in 0..=5 {
                    edges.push(hx(getter(node, Edge::from_raw(e).unwrap())?));
                }
                parts.push(edges.join(" "));
            }
            parts.push(format!("{} {}", node.has_new_layout()? as u32, node.is_dirty()? as u32));
            Ok(())
        };
        if let Err(error) = row() {
            parts.push(err_text(&error));
        }
        self.out.borrow_mut().push(parts.join(" | "));
        for index in 0..node.get_child_count() {
            self.dump(&node.get_child(index).unwrap().unwrap());
        }
    }

    fn exec(&mut self, op: &str) {
        let tokens: Vec<&str> = op.split(' ').collect();
        let (name, args) = (tokens[0], &tokens[1..]);
        let n = |i: usize| self.nodes[int(args[i]) as usize].clone().unwrap();
        let c = |i: usize| self.configs[int(args[i]) as usize].clone().unwrap();
        let out = self.out.clone();
        let push = |line: String| out.borrow_mut().push(line);
        let ts_op = |result: Result<()>| push(result.map_or_else(|error| err_text(&error), |()| "ok".to_owned()));
        let ts_val = |result: Result<String>| push(result.unwrap_or_else(|error| err_text(&error)));
        let raw_op = |result: Result<()>| {
            push(match result {
                Ok(()) => "s=0".to_owned(),
                Err(Error::Yoga { status, .. }) => format!("s={}", status as u32),
                Err(other) => panic!("{other}"),
            })
        };
        let raw_val = |result: Result<String>| {
            push(match result {
                Ok(text) => format!("s=0 {text}"),
                Err(Error::Yoga { status, .. }) => format!("s={}", status as u32),
                Err(other) => panic!("{other}"),
            })
        };
        match name {
            "cfg" => {
                let index = int(args[0]) as usize;
                if self.configs.len() <= index {
                    self.configs.resize(index + 1, None);
                }
                self.configs[index] = Some(Config::create().unwrap());
                push("ok".to_owned());
            }
            "cfg.web" => ts_op(c(0).set_use_web_defaults(args[1] == "1")),
            "cfg.scale" => ts_op(c(0).set_point_scale_factor(number(args[1]))),
            "cfg.errata" => ts_op(c(0).set_errata(Errata(int(args[1])))),
            "cfg.exp" => {
                ts_op(c(0).set_experimental_feature_enabled(ExperimentalFeature::WebFlexBasis, args[1] == "1"))
            }
            "cfg.rounds" => ts_op(c(0).set_rounds_positions_from_absolute_edges(args[1] == "1")),
            "cfg.get" => {
                let config = c(0);
                push(format!(
                    "{} {} {} {}",
                    config.use_web_defaults() as u32,
                    hx(config.get_point_scale_factor()),
                    config.get_errata().0,
                    config.is_experimental_feature_enabled(ExperimentalFeature::WebFlexBasis) as u32
                ));
            }
            "new" => {
                let id = int(args[0]) as usize;
                let node = match args[1] {
                    "-1" => Node::create(None),
                    "-2" => Node::create_for_opentui(),
                    _ => Node::create(Some(&c(1))),
                }
                .unwrap();
                if self.nodes.len() <= id {
                    self.nodes.resize(id + 1, None);
                }
                self.ids.insert(node.clone(), id);
                self.nodes[id] = Some(node);
                push("ok".to_owned());
            }
            "ins" => ts_op(n(0).insert_child(&n(1), int(args[2]))),
            "rm" => ts_op(n(0).remove_child(&n(1))),
            "rmall" => ts_op(n(0).remove_all_children()),
            "free" => ts_op(n(0).free()),
            "freerec" => ts_op(n(0).free_recursive()),
            "reset" => ts_op(n(0).reset()),
            "copy" => ts_op(n(0).copy_style(&n(1))),
            "mv" => raw_op(n(0).move_child(&n(1), int(args[2]))),
            "child" => ts_val(n(0).get_child(int(args[1])).map(|child| format!("child {}", self.id(child)))),
            "count" => push(n(0).get_child_count().to_string()),
            "parent" => push(self.id(n(0).get_parent())),
            "dirty" => ts_val(n(0).is_dirty().map(|v| format!("dirty {}", v as u32))),
            "newl" => ts_val(n(0).has_new_layout().map(|v| format!("newl {}", v as u32))),
            "seen" => ts_op(n(0).mark_layout_seen()),
            "mark" => ts_op(n(0).mark_dirty()),
            "hasm" => push((n(0).has_measure_func() as u32).to_string()),
            "lay" => {
                let direction = Direction::from_raw(int(args[3])).unwrap();
                ts_op(n(0).calculate_layout(layout_input(args[1]), layout_input(args[2]), direction))
            }
            "layr" => {
                let (w, h) = (number(args[1]) as f32, number(args[2]) as f32);
                raw_op(n(0).calculate_layout_checked(w, h, int(args[3])))
            }
            "dump" => self.dump(&n(0)),
            "get" => push(getters(&n(0))),
            "se" => raw_op(n(0).style_set_enum(int(args[1]), int(args[2]))),
            "sf" => raw_op(n(0).style_set_float(int(args[1]), number(args[2]) as f32)),
            "sb" => raw_op(n(0).style_set_border(int(args[1]), number(args[2]) as f32)),
            "sv" => raw_op(n(0).style_set_value(int(args[1]), int(args[2]), int(args[3]), number(args[4]) as f32)),
            "sd" => raw_op(n(0).style_set_dimension(int(args[1]), int(args[2]), number(args[3]) as f32, int(args[4]))),
            "sp" => {
                let mut units = [0u32; 4];
                let mut values = [0f32; 4];
                for e in 0..4 {
                    units[e] = int(args[2 + e * 2]);
                    values[e] = number(args[3 + e * 2]) as f32;
                }
                raw_op(n(0).style_set_positions(int(args[1]), units, values))
            }
            "flag" => {
                let (node, value) = (n(0), args[2] == "1");
                raw_op(match args[1] {
                    "0" => node.set_has_new_layout(value),
                    "1" => node.set_is_reference_baseline(value),
                    _ => node.set_always_forms_containing_block(value),
                })
            }
            "ge" => raw_val(n(0).style_get_enum(int(args[1])).map(|v| v.to_string())),
            "gf" => raw_val(n(0).style_get_float(int(args[1])).map(hx)),
            "gb" => raw_val(n(0).style_get_border(int(args[1])).map(hx)),
            "gv" => raw_val(n(0).style_get_value(int(args[1]), int(args[2])).map(value)),
            "gl" => raw_val(n(0).layout_get_edge(int(args[1]), int(args[2])).map(hx)),
            "ts" => ts_op(ts_call(&n(0), args[1], &args[2..])),
            "meas" => {
                let f = measure_function(&self.out, int(args[0]) as usize, &args[1..]);
                ts_op(n(0).set_measure_func(Some(f)))
            }
            "unmeas" => ts_op(n(0).unset_measure_func()),
            "nmeas" => {
                let cap = args[1] == "1";
                let f = (args.len() > 2).then(|| measure_function(&self.out, int(args[0]) as usize, &args[2..]));
                raw_op(n(0).set_native_measure_func(f, cap))
            }
            "dirt" => {
                let id = int(args[0]);
                let sink = self.out.clone();
                ts_op(n(0).set_dirtied_func(Some(Box::new(move |_| sink.borrow_mut().push(format!("d {id}"))))))
            }
            "undirt" => ts_op(n(0).unset_dirtied_func()),
            "inval" => ts_op(n(0).invalidate_measure()),
            "forget" => ts_op(n(0).forget_measurements()),
            _ => panic!("op {op}"),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let programs = std::fs::read_to_string(&args[1]).expect("programs");
    let out: Out = Rc::new(RefCell::new(Vec::new()));
    let mut results = String::new();
    let mut state: Option<State> = None;
    for line in programs.lines() {
        if line == "end" {
            drop(state.take());
            for answer in out.borrow_mut().drain(..) {
                results.push_str(&answer);
                results.push('\n');
            }
            results.push_str("end\n");
            continue;
        }
        let state = state.get_or_insert_with(|| State {
            configs: Vec::new(),
            nodes: Vec::new(),
            ids: HashMap::new(),
            out: out.clone(),
        });
        state.exec(line);
    }
    std::fs::write(&args[2], results).expect("results");
}
