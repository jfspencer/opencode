//! The Rust side of harness/diff.ts: reads one JSON case per line (`{"m": widthMethod, "ops": [[op, ...args]]}`)
//! from the file named by the first argument and writes one JSON array of op results per line to the second. The
//! op vocabulary is harness/ops.ts's; both sides must answer each op identically.

use opencode_tui_text::text::HighlightRange;
use opencode_tui_text::{
    LineInfo, SelectionBehavior, SelectionOccupancy, StyleSpec, StyledChunk, SyntaxStyle, TextAlign, TextBuffer,
    TextBufferView, TextContext, TextError, WidthMethod, WrapMode,
};
use std::fmt::Write as _;
use std::io::{BufRead, Write};

mod draw;

mod json {
    #[derive(Clone, Debug)]
    pub enum Value {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Value>),
        Obj(Vec<(String, Value)>),
    }

    impl Value {
        pub fn str(&self) -> &str {
            match self {
                Value::Str(s) => s,
                _ => panic!("not a string: {self:?}"),
            }
        }
        pub fn num(&self) -> f64 {
            match self {
                Value::Num(n) => *n,
                _ => panic!("not a number: {self:?}"),
            }
        }
        pub fn u32(&self) -> u32 {
            self.num() as u32
        }
        pub fn i32(&self) -> i32 {
            self.num() as i32
        }
        pub fn bool(&self) -> bool {
            match self {
                Value::Bool(b) => *b,
                _ => panic!("not a bool: {self:?}"),
            }
        }
        pub fn arr(&self) -> &[Value] {
            match self {
                Value::Arr(a) => a,
                _ => panic!("not an array: {self:?}"),
            }
        }
        pub fn is_null(&self) -> bool {
            matches!(self, Value::Null)
        }
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }
    }

    pub fn parse(text: &str) -> Value {
        let mut p = Parser { s: text.as_bytes(), i: 0 };
        p.value()
    }

    struct Parser<'a> {
        s: &'a [u8],
        i: usize,
    }

    impl Parser<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\n' | b'\r' | b'\t') {
                self.i += 1;
            }
        }
        fn value(&mut self) -> Value {
            self.ws();
            match self.s[self.i] {
                b'n' => {
                    self.i += 4;
                    Value::Null
                }
                b't' => {
                    self.i += 4;
                    Value::Bool(true)
                }
                b'f' => {
                    self.i += 5;
                    Value::Bool(false)
                }
                b'"' => Value::Str(self.string()),
                b'[' => {
                    self.i += 1;
                    let mut items = Vec::new();
                    loop {
                        self.ws();
                        if self.s[self.i] == b']' {
                            self.i += 1;
                            break;
                        }
                        items.push(self.value());
                        self.ws();
                        if self.s[self.i] == b',' {
                            self.i += 1;
                        }
                    }
                    Value::Arr(items)
                }
                b'{' => {
                    self.i += 1;
                    let mut fields = Vec::new();
                    loop {
                        self.ws();
                        if self.s[self.i] == b'}' {
                            self.i += 1;
                            break;
                        }
                        let key = self.string();
                        self.ws();
                        self.i += 1;
                        let value = self.value();
                        fields.push((key, value));
                        self.ws();
                        if self.s[self.i] == b',' {
                            self.i += 1;
                        }
                    }
                    Value::Obj(fields)
                }
                _ => {
                    let start = self.i;
                    while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                        self.i += 1;
                    }
                    Value::Num(std::str::from_utf8(&self.s[start..self.i]).unwrap().parse().unwrap())
                }
            }
        }
        fn string(&mut self) -> String {
            self.i += 1;
            let mut out = String::new();
            let mut pending_high: Option<u32> = None;
            loop {
                let b = self.s[self.i];
                match b {
                    b'"' => {
                        self.i += 1;
                        break;
                    }
                    b'\\' => {
                        let e = self.s[self.i + 1];
                        self.i += 2;
                        let c = match e {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            b'b' => '\u{8}',
                            b'f' => '\u{c}',
                            b'/' => '/',
                            b'\\' => '\\',
                            b'"' => '"',
                            b'u' => {
                                let hex = std::str::from_utf8(&self.s[self.i..self.i + 4]).unwrap();
                                self.i += 4;
                                let unit = u32::from_str_radix(hex, 16).unwrap();
                                if (0xD800..0xDC00).contains(&unit) {
                                    pending_high = Some(unit);
                                    continue;
                                }
                                if (0xDC00..0xE000).contains(&unit) {
                                    let high = pending_high.take().expect("a surrogate pair");
                                    char::from_u32(0x10000 + ((high - 0xD800) << 10) + (unit - 0xDC00)).unwrap()
                                } else {
                                    char::from_u32(unit).unwrap()
                                }
                            }
                            _ => panic!("bad escape"),
                        };
                        out.push(c);
                    }
                    _ => {
                        let len = match b {
                            0x00..=0x7F => 1,
                            0xC0..=0xDF => 2,
                            0xE0..=0xEF => 3,
                            _ => 4,
                        };
                        out.push_str(std::str::from_utf8(&self.s[self.i..self.i + len]).unwrap());
                        self.i += len;
                    }
                }
            }
            out
        }
    }

    pub fn write_str(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 || c == '\u{7f}' => {
                    use std::fmt::Write;
                    write!(out, "\\u{:04x}", c as u32).unwrap();
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
}

use json::Value;

fn nums(out: &mut String, values: &[u32]) {
    out.push('[');
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write!(out, "{v}").unwrap();
    }
    out.push(']');
}

fn line_info(out: &mut String, info: &LineInfo) {
    out.push('[');
    nums(out, &info.line_start_cols);
    out.push(',');
    nums(out, &info.line_width_cols);
    out.push(',');
    nums(out, &info.line_sources);
    out.push(',');
    nums(out, &info.line_wraps);
    write!(out, ",{}]", info.line_width_cols_max).unwrap();
}

fn spans(out: &mut String, grid: &draw::Grid) {
    out.push('[');
    for (i, line) in grid.span_lines().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, (text, fg, bg, attributes, width)) in line.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push('[');
            json::write_str(out, text);
            write!(out, ",[{},{},{},{}],[{},{},{},{}],{attributes},{width}]", fg[0], fg[1], fg[2], fg[3], bg[0], bg[1], bg[2], bg[3]).unwrap();
        }
        out.push(']');
    }
    out.push(']');
}

fn error(out: &mut String, e: TextError) {
    write!(out, "{{\"err\":\"{}\"}}", e.status()).unwrap();
}

fn unit(out: &mut String, r: Result<(), TextError>) {
    match r {
        Ok(()) => out.push_str("null"),
        Err(e) => error(out, e),
    }
}

fn color(v: Option<&Value>) -> Option<[u16; 4]> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    let a = v.arr();
    Some([a[0].u32() as u16, a[1].u32() as u16, a[2].u32() as u16, a[3].u32() as u16])
}

fn wrap_mode(s: &str) -> WrapMode {
    match s {
        "none" => WrapMode::None,
        "char" => WrapMode::Char,
        _ => WrapMode::Word,
    }
}

fn behavior(s: &str) -> SelectionBehavior {
    match s {
        "word" => SelectionBehavior::Word,
        "line" => SelectionBehavior::Line,
        _ => SelectionBehavior::Cell,
    }
}

fn highlight(v: &Value) -> HighlightRange {
    let a = v.arr();
    HighlightRange { start: a[0].u32(), end: a[1].u32(), style_id: a[2].u32(), priority: a[3].u32(), hl_ref: a[4].u32() }
}

fn spec(v: &Value) -> StyleSpec {
    let flag = |k: &str| v.get(k).map(|b| b.bool());
    StyleSpec { fg: color(v.get("fg")), bg: color(v.get("bg")), bold: flag("bold"), italic: flag("italic"), underline: flag("underline"), dim: flag("dim") }
}

struct TextState {
    ctx: TextContext,
    tb: TextBuffer,
    view: TextBufferView,
    style: SyntaxStyle,
}

fn text_op(state: &mut TextState, op: &[Value], out: &mut String) {
    let name = op[0].str();
    let tb = &state.tb;
    let v = &state.view;
    match name {
        "setText" => match tb.set_text(op[1].str()) {
            Ok(_) => out.push_str("null"),
            Err(e) => error(out, e),
        },
        "append" => unit(out, tb.append(op[1].str())),
        "setStyledText" => {
            let chunks: Vec<StyledChunk> = op[1]
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
            match tb.set_styled_text(&chunks) {
                Ok(_) => out.push_str("null"),
                Err(e) => error(out, e),
            }
        }
        "clear" => {
            tb.clear();
            out.push_str("null");
        }
        "reset" => {
            tb.reset();
            out.push_str("null");
        }
        "tabWidth" => unit(out, tb.set_tab_width(op[1].u32())),
        "addHighlight" => unit(out, tb.add_highlight(op[1].u32(), highlight(&op[2]))),
        "addHighlightByCharRange" => unit(out, tb.add_highlight_by_char_range(highlight(&op[1]))),
        "removeHighlightsByRef" => unit(out, tb.remove_highlights_by_ref(op[1].u32())),
        "clearLineHighlights" => {
            tb.clear_line_highlights(op[1].u32());
            out.push_str("null");
        }
        "clearAllHighlights" => {
            tb.clear_all_highlights();
            out.push_str("null");
        }
        "registerStyle" => {
            let id = state.style.register_style(op[1].str(), spec(&op[2]));
            write!(out, "{id}").unwrap();
        }
        "syntaxStyle" => {
            tb.set_syntax_style(if op[1].bool() { Some(&state.style) } else { None });
            out.push_str("null");
        }
        "info" => write!(
            out,
            "[{},{},{},{},{}]",
            tb.length(),
            tb.byte_size(),
            tb.get_line_count(),
            tb.get_highlight_count(),
            tb.get_tab_width()
        )
        .unwrap(),
        "text" => json::write_str(out, &tb.get_plain_text()),
        "range" => json::write_str(out, &tb.get_text_range(op[1].u32(), op[2].u32())),
        "hl" => {
            out.push('[');
            for (i, h) in tb.get_line_highlights(op[1].u32()).iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write!(out, "[{},{},{},{},{}]", h.start, h.end, h.style_id, h.priority, h.hl_ref).unwrap();
            }
            out.push(']');
        }
        "wrapMode" => {
            v.set_wrap_mode(wrap_mode(op[1].str()));
            out.push_str("null");
        }
        "wrapWidth" => unit(out, v.set_wrap_width(if op[1].is_null() { None } else { Some(op[1].u32()) })),
        "viewport" => unit(out, v.set_viewport(op[1].u32(), op[2].u32(), op[3].u32(), op[4].u32())),
        "viewportSize" => unit(out, v.set_viewport_size(op[1].u32(), op[2].u32())),
        "firstLineOffset" => unit(out, v.set_first_line_offset(op[1].u32())),
        "truncate" => {
            v.set_truncate(op[1].bool());
            out.push_str("null");
        }
        "align" => {
            v.set_text_align(match op[1].str() {
                "center" => TextAlign::Center,
                "right" => TextAlign::Right,
                _ => TextAlign::Left,
            });
            out.push_str("null");
        }
        "tabIndicator" => unit(out, v.set_tab_indicator(op[1].u32())),
        "lineInfo" => line_info(out, &v.line_info()),
        "logical" => line_info(out, &v.logical_line_info()),
        "sources" => nums(out, &v.get_line_sources(op[1].u32(), op[2].u32())),
        "measure" => match v.measure_for_dimensions(op[1].u32(), op[2].u32()) {
            Ok(m) => write!(out, "[{},{}]", m.line_count, m.width_cols_max).unwrap(),
            Err(e) => error(out, e),
        },
        "vcount" => write!(out, "{}", v.get_virtual_line_count()).unwrap(),
        "sel" => {
            v.set_selection(op[1].u32(), op[2].u32(), color(op.get(3)), color(op.get(4)));
            out.push_str("null");
        }
        "usel" => {
            v.update_selection(op[1].u32(), color(op.get(2)), color(op.get(3)));
            out.push_str("null");
        }
        "rsel" => {
            v.reset_selection();
            out.push_str("null");
        }
        "lsel" | "ulsel" => {
            let f = if name == "lsel" { TextBufferView::set_local_selection } else { TextBufferView::update_local_selection };
            match f(v, op[1].i32(), op[2].i32(), op[3].i32(), op[4].i32(), None, None, behavior(op[5].str())) {
                Ok(b) => write!(out, "{b}").unwrap(),
                Err(e) => error(out, e),
            }
        }
        "rlsel" => {
            v.reset_local_selection();
            out.push_str("null");
        }
        "occ" => {
            v.set_selection_occupancy(if op[1].str() == "boundary" { SelectionOccupancy::Boundary } else { SelectionOccupancy::Cell });
            out.push_str("null");
        }
        "selinfo" => {
            out.push('[');
            match v.get_selection() {
                Some((s, e)) => write!(out, "[{s},{e}]").unwrap(),
                None => out.push_str("null"),
            }
            out.push(',');
            json::write_str(out, &v.get_selected_text());
            let occ = match v.get_selection_occupancy() {
                SelectionOccupancy::Cell => "cell",
                SelectionOccupancy::Boundary => "boundary",
            };
            write!(out, ",\"{occ}\"]").unwrap();
        }
        "defaults" => unit(out, tb.set_all_defaults(color(op.get(1)), color(op.get(2)), if op[3].is_null() { None } else { Some(op[3].u32()) })),
        "tabColor" => {
            v.set_tab_indicator_color(color(op.get(1)));
            out.push_str("null");
        }
        "draw" => {
            let (bw, bh, x, y) = (op[1].u32(), op[2].u32(), op[3].i32(), op[4].i32());
            let mut grid = draw::Grid::new(bw, bh, [0, 0, 0, 255]);
            v.draw(y, |src| draw::draw(src, &mut grid, x, y));
            spans(out, &grid);
        }
        "newView" => {
            state.view = TextBufferView::create(&state.tb);
            out.push_str("null");
        }
        _ => panic!("unknown op {name}"),
    }
}

fn run_case(case: &Value, out: &mut String) {
    let method = WidthMethod::parse(case.get("m").unwrap().str()).unwrap();
    let ctx = TextContext::new();
    let tb = TextBuffer::create(method, &ctx);
    let view = TextBufferView::create(&tb);
    let mut state = TextState { ctx, tb, view, style: SyntaxStyle::create() };
    out.push('[');
    for (i, op) in case.get("ops").unwrap().arr().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        text_op(&mut state, op.arr(), out);
    }
    out.push(']');
    let _ = &state.ctx;
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = std::io::BufReader::new(std::fs::File::open(&args[1]).expect("input"));
    let mut output = std::io::BufWriter::new(std::fs::File::create(&args[2]).expect("output"));
    let mut out = String::new();
    for line in input.lines() {
        let line = line.unwrap();
        if line.is_empty() {
            continue;
        }
        out.clear();
        run_case(&json::parse(&line), &mut out);
        out.push('\n');
        output.write_all(out.as_bytes()).unwrap();
    }
}
