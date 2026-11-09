//! The Rust side of the differential harness (harness/run.sh): runs the op scripts of harness/ops.ts and prints
//! what harness/ops.ts, frames.ts and setup.ts print for the TS reference.
//!
//! usage: harness buffer <script> | harness frames <script> | harness setup <log>
use std::collections::HashMap;

use opencode_tui_core::border::{BorderSides, BorderStyle, TitleAlignment};
use opencode_tui_core::buffer::{DrawBoxOptions, OptimizedBuffer, TextSelection};
use opencode_tui_core::color::RGBA;
use opencode_tui_core::output::Renderer;
use opencode_tui_core::pool;
use opencode_tui_core::terminal::{Control, CursorControl, CursorStyle, MousePointerStyle, Pump, RemoteMode, Replies, Session, Terminal, TerminalOptions};
use opencode_tui_core::unicode::WidthMethod;

fn color(token: &str) -> Option<RGBA> {
    if token == "-" {
        return None;
    }
    let hex = &token[1..];
    let mut parts = [0u16; 4];
    for (i, part) in parts.iter_mut().enumerate() {
        *part = u16::from_str_radix(&hex[i * 4..i * 4 + 4], 16).expect("color");
    }
    Some(RGBA(parts))
}

fn text(token: &str) -> String {
    let bytes: Vec<u8> = (0..token.len()).step_by(2).map(|i| u8::from_str_radix(&token[i..i + 2], 16).expect("hex")).collect();
    String::from_utf8(bytes).expect("utf8")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn alignment(token: &str) -> TitleAlignment {
    match token {
        "1" => TitleAlignment::Center,
        "2" => TitleAlignment::Right,
        _ => TitleAlignment::Left,
    }
}

/// Draws one op (see ops.ts). `buffers` holds the other buffers of the case for `fb`.
fn draw(target: &mut OptimizedBuffer, op: &[&str], sources: &[&OptimizedBuffer]) {
    let n = |i: usize| op[i].parse::<f64>().expect("number");
    let i = |i: usize| n(i) as i32;
    match op[0] {
        "clear" => target.clear(color(op[1]).unwrap()),
        "cell" | "cellblend" | "char" => {
            let cp = n(3) as u32;
            let (fg, bg, attrs) = (color(op[4]).unwrap(), color(op[5]).unwrap(), n(6) as u32);
            match op[0] {
                "cell" => target.set_cell(i(1), i(2), char::from_u32(cp).unwrap(), fg, bg, attrs),
                "cellblend" => target.set_cell_with_alpha_blending(i(1), i(2), char::from_u32(cp).unwrap(), fg, bg, attrs),
                _ => target.draw_char(cp, i(1), i(2), fg, bg, attrs),
            }
        }
        "text" => target.draw_text(&text(op[1]), i(2), i(3), color(op[4]).unwrap(), color(op[5]), n(6) as u32, None),
        "seltext" => {
            let selection = TextSelection { start: n(7) as usize, end: n(8) as usize, bg_color: color(op[9]), fg_color: color(op[10]) };
            target.draw_text(&text(op[1]), i(2), i(3), color(op[4]).unwrap(), color(op[5]), n(6) as u32, Some(&selection));
        }
        "fit" => target.draw_text_fit(&text(op[1]), i(2), i(3), i(4), color(op[5]).unwrap(), color(op[6]), op[7] == "1"),
        "fill" => target.fill_rect(i(1), i(2), i(3), i(4), color(op[5]).unwrap()),
        "box" => {
            let (style, custom) = match op[5] {
                "single" => (BorderStyle::Single, None),
                "double" => (BorderStyle::Double, None),
                "rounded" => (BorderStyle::Rounded, None),
                "heavy" => (BorderStyle::Heavy, None),
                list => {
                    let mut chars = [0u32; 11];
                    for (slot, value) in chars.iter_mut().zip(list.split(',')) {
                        *slot = value.parse().expect("code point");
                    }
                    (BorderStyle::Single, Some(chars))
                }
            };
            let title = (op[10] != "-").then(|| text(op[10]));
            let bottom = (op[13] != "-").then(|| text(op[13]));
            target.draw_box(&DrawBoxOptions {
                x: i(1),
                y: i(2),
                width: i(3),
                height: i(4),
                border_style: style,
                custom_border_chars: custom,
                border: BorderSides::from_bits(n(6) as u32),
                border_color: color(op[7]).unwrap(),
                background_color: color(op[8]).unwrap(),
                should_fill: op[9] == "1",
                title: title.as_deref(),
                title_color: color(op[11]),
                title_alignment: alignment(op[12]),
                bottom_title: bottom.as_deref(),
                bottom_title_alignment: alignment(op[14]),
            });
        }
        "pscissor" => target.push_scissor_rect(i(1), i(2), i(3), i(4)),
        "popscissor" => target.pop_scissor_rect(),
        "clrscissor" => target.clear_scissor_rects(),
        "popacity" => target.push_opacity(n(1) as f32),
        "popopacity" => target.pop_opacity(),
        "clropacity" => target.clear_opacity(),
        "resize" => target.resize(n(1) as u32, n(2) as u32),
        "respect" => target.set_respect_alpha(op[1] == "1"),
        "fb" => {
            let opt = |k: usize| (op[k] != "-").then(|| n(k) as u32);
            let source = sources[n(1) as usize];
            target.draw_frame_buffer(i(2), i(3), source, opt(4), opt(5), opt(6), opt(7));
        }
        other => panic!("unknown op {other}"),
    }
}

fn dump(buffer: &OptimizedBuffer) -> Vec<String> {
    let mut rows = Vec::new();
    for y in 0..buffer.height() {
        let mut row = Vec::new();
        for x in 0..buffer.width() {
            let cell = buffer.get(x, y).unwrap();
            let token = if pool::is_grapheme_char(cell.char) {
                let bytes = pool::grapheme_bytes(pool::grapheme_id_from_char(cell.char)).unwrap_or_else(|| b" ".to_vec());
                format!("g{}_{}", pool::char_right_extent(cell.char) + 1, hex(&bytes))
            } else if pool::is_continuation_char(cell.char) {
                format!("c{}", pool::char_left_extent(cell.char))
            } else {
                format!("u{:x}", cell.char)
            };
            let rgba = |c: RGBA| c.0.iter().map(|v| format!("{v:04x}")).collect::<String>();
            row.push(format!("{token}:{}:{}:{:x}", rgba(cell.fg), rgba(cell.bg), cell.attributes));
        }
        rows.push(row.join(" "));
    }
    rows
}

fn run_buffers(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buffers: Vec<OptimizedBuffer> = Vec::new();
    let mut target = 0usize;
    let mut name = String::new();
    for line in script.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let op: Vec<&str> = line.split(' ').collect();
        match op[0] {
            "case" => {
                buffers.clear();
                name = op[1].to_owned();
            }
            "buffer" => {
                buffers.push(OptimizedBuffer::new(op[1].parse().unwrap(), op[2].parse().unwrap(), op[3] == "1", WidthMethod::Unicode));
                target = buffers.len() - 1;
            }
            "target" => target = op[1].parse().unwrap(),
            "dump" => {
                let buffer = &buffers[target];
                out.push(format!("dump {name} {} {}", buffer.width(), buffer.height()));
                out.extend(dump(buffer));
            }
            _ => {
                // The target leaves the list while it draws (the scripts never draw a buffer onto itself).
                let mut current = std::mem::replace(&mut buffers[target], OptimizedBuffer::new(1, 1, false, WidthMethod::Unicode));
                let sources: Vec<&OptimizedBuffer> = buffers.iter().collect();
                draw(&mut current, &op, &sources);
                buffers[target] = current;
            }
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = std::fs::read_to_string(&args[2]).expect("input");
    let out = match args[1].as_str() {
        "buffer" => run_buffers(&input),
        "frames" => run_frames(&input),
        "setup" => run_setup(&input),
        other => panic!("unknown mode {other}"),
    };
    let mut text = out.join("\n");
    text.push('\n');
    use std::io::Write;
    std::io::stdout().write_all(text.as_bytes()).unwrap();
}

/// The environment the TS side runs its renderer with (frames.ts, setup.ts).
fn environment() -> HashMap<String, String> {
    HashMap::from([("TERM".to_owned(), "xterm-256color".to_owned())])
}

/// Frame scripts (frames.ts): `case NAME W H`, `reply HEX` (a terminal reply during setup), `settle` (setup done),
/// `frame`, ops, `endframe`, `resize W H`. Prints each frame's bytes in hex.
fn run_frames(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut state: Option<(Renderer, Session, Replies)> = None;
    let mut bytes = Vec::new();
    let mut name = String::new();
    let mut now = 0u64;
    for line in script.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let op: Vec<&str> = line.split(' ').collect();
        match op[0] {
            "case" => {
                name = op[1].to_owned();
                let renderer = Renderer::new(op[2].parse().unwrap(), op[3].parse().unwrap(), Terminal::new(RemoteMode::Local, Some(environment())));
                state = Some((renderer, Session::new(), Replies::new()));
                let (renderer, session, replies) = state.as_mut().unwrap();
                replies.before_setup(renderer);
                session.setup_terminal(renderer, TerminalOptions::default()).unwrap();
                bytes.clear();
                while session.pump(renderer, now, &mut bytes) == Pump::Again {}
                replies.after_setup(session, renderer, now, &mut bytes);
            }
            "reply" => {
                let (renderer, session, replies) = state.as_mut().unwrap();
                feed_reply(&text(op[1]), renderer, session, replies, now, &mut bytes);
            }
            "settle" => {
                let (renderer, session, replies) = state.as_mut().unwrap();
                now += 10_000;
                replies.tick(session, renderer, now, &mut bytes);
                bytes.clear();
                // The TS side renders once after setup settles (frames.ts); that frame is not compared.
                renderer.render(true, &mut bytes);
                bytes.clear();
            }
            "frame" => {}
            "endframe" => {
                let (renderer, ..) = state.as_mut().unwrap();
                renderer.render(false, &mut bytes);
                out.push(format!("frame {name} {}", hex(&bytes)));
                bytes.clear();
            }
            "resize" => {
                let (renderer, ..) = state.as_mut().unwrap();
                renderer.resize(op[1].parse().unwrap(), op[2].parse().unwrap());
            }
            "pointer" => {
                let (renderer, session, _) = state.as_mut().unwrap();
                let pointer = MousePointerStyle::from_name(op[1]);
                session.control(renderer, Control::Cursor(CursorControl { pointer, ..Default::default() }), &mut bytes).unwrap();
            }
            "cursor" => {
                let (renderer, session, _) = state.as_mut().unwrap();
                let style = match op[4] {
                    "block" => Some(CursorStyle::Block),
                    "line" => Some(CursorStyle::Line),
                    "underline" => Some(CursorStyle::Underline),
                    "default" => Some(CursorStyle::Default),
                    _ => None,
                };
                let blinking = (op[5] != "-").then(|| op[5] == "1");
                let cursor_color = color(op[6]);
                if style.is_some() || blinking.is_some() || cursor_color.is_some() {
                    let control = CursorControl { style, blinking, color: cursor_color, ..Default::default() };
                    session.control(renderer, Control::Cursor(control), &mut bytes).unwrap();
                }
                let position = Some((op[1].parse().unwrap(), op[2].parse().unwrap(), op[3] == "1"));
                session.control(renderer, Control::Cursor(CursorControl { position, ..Default::default() }), &mut bytes).unwrap();
            }
            _ => {
                let (renderer, ..) = state.as_mut().unwrap();
                draw(renderer.next_buffer(), &op, &[]);
            }
        }
    }
    out
}

/// Splits a reply chunk into sequences and hands each to the renderer's handlers, as the stdin parser and
/// renderer.ts do (a stand-in for the parser: CSI, OSC, DCS and APC).
fn feed_reply(chunk: &str, renderer: &mut Renderer, session: &mut Session, replies: &mut Replies, now: u64, out: &mut Vec<u8>) {
    for sequence in split_sequences(chunk) {
        if sequence.starts_with("\x1b]") {
            replies.handle_osc(&sequence, session, renderer, now, out);
        }
        let cpr = sequence.starts_with("\x1b[") && sequence.ends_with('R') && !sequence.starts_with("\x1b[?");
        if cpr && replies.handle_cpr(&sequence, session, renderer, out).handled {
            continue;
        }
        replies.handle_sequence(&sequence, session, renderer, now, out);
    }
}

fn split_sequences(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b || i + 1 >= bytes.len() {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != 0x1b {
                i += 1;
            }
            out.push(text[start..i].to_owned());
            continue;
        }
        let start = i;
        match bytes[i + 1] {
            b'[' => {
                i += 2;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1;
            }
            b']' | b'P' | b'_' => {
                i += 2;
                while i < bytes.len() && bytes[i] != 0x07 && !(bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\')) {
                    i += 1;
                }
                i += if i < bytes.len() && bytes[i] == 0x07 { 1 } else { 2 };
            }
            _ => i += 2,
        }
        out.push(text[start..i.min(bytes.len())].to_owned());
    }
    out
}

/// Setup logs (setup.ts): `case NAME W H`, `reply HEX`, `wait MS`. Prints the control bytes (frames excluded)
/// written from the start up to the end, in hex, one line per step.
fn run_setup(log: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut state: Option<(Renderer, Session, Replies)> = None;
    let mut now = 0u64;
    let mut name = String::new();
    let mut bytes = Vec::new();
    for line in log.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let op: Vec<&str> = line.split(' ').collect();
        match op[0] {
            "case" => {
                name = op[1].to_owned();
                now = 0;
                let env: HashMap<String, String> = op[5]
                    .split(',')
                    .filter_map(|pair| pair.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
                    .collect();
                let renderer = Renderer::new(op[2].parse().unwrap(), op[3].parse().unwrap(), Terminal::new(RemoteMode::Local, Some(env)));
                state = Some((renderer, Session::new(), Replies::new()));
                let (renderer, session, replies) = state.as_mut().unwrap();
                replies.before_setup(renderer);
                let options = TerminalOptions { mouse: op[4] == "1", ..TerminalOptions::default() };
                session.setup_terminal(renderer, options).unwrap();
                while session.pump(renderer, now, &mut bytes) == Pump::Again {}
                replies.after_setup(session, renderer, now, &mut bytes);
            }
            "reply" => {
                let (renderer, session, replies) = state.as_mut().unwrap();
                feed_reply(&text(op[1]), renderer, session, replies, now, &mut bytes);
            }
            "wait" => {
                let (renderer, session, replies) = state.as_mut().unwrap();
                let until = now + op[1].parse::<u64>().unwrap();
                while let Some(deadline) = replies.next_deadline().filter(|d| *d <= until) {
                    now = deadline;
                    replies.tick(session, renderer, now, &mut bytes);
                }
                now = until;
            }
            "close" => {
                let (renderer, session, _) = state.as_mut().unwrap();
                session.begin_close();
                loop {
                    match session.pump(renderer, now, &mut bytes) {
                        Pump::Again => {}
                        Pump::WaitUntil(t) => now = t,
                        Pump::Idle | Pump::Closed => break,
                    }
                }
            }
            "end" => {
                out.push(format!("setup {name} {}", hex(&bytes)));
                bytes.clear();
            }
            other => panic!("unknown setup op {other}"),
        }
    }
    out
}
