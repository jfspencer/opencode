//! The Rust side of harness/oracle.ts: replays its programs (one op per line, `end` after each) through
//! opencode-tui-input on a manual clock and writes the answers in the oracle's format.
//!
//! usage: input-driver <programs> <results>
use opencode_tui_input::*;
use std::cell::Cell;
use std::fmt::Write as _;
use std::rc::Rc;

fn num(x: f64) -> String {
    format!("{:016x}", x.to_bits())
}

fn unhex(text: &str) -> Vec<u8> {
    if text == "-" {
        return Vec::new();
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

fn hex(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "-".to_owned();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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

fn opt(value: Option<bool>) -> &'static str {
    match value {
        None => "-",
        Some(true) => "1",
        Some(false) => "0",
    }
}

fn flag(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn key(k: &ParsedKey) -> String {
    [
        format!("name={}", esc(&k.name)),
        format!(
            "ctrl={} meta={} shift={} option={} number={}",
            flag(k.ctrl),
            flag(k.meta),
            flag(k.shift),
            flag(k.option),
            flag(k.number)
        ),
        format!("seq={}", esc(&k.sequence)),
        format!("raw={}", esc(&k.raw)),
        format!(
            "type={} source={} code={}",
            k.event_type.as_str(),
            k.source.as_str(),
            k.code.as_deref().map_or("-".to_owned(), esc)
        ),
        format!("super={} hyper={} caps={} num={}", opt(k.super_), opt(k.hyper), opt(k.caps_lock), opt(k.num_lock)),
        format!("base={} repeated={}", k.base_code.map_or("-".to_owned(), |code| code.to_string()), opt(k.repeated)),
    ]
    .join(" ")
}

fn mouse(m: &RawMouseEvent) -> String {
    let scroll =
        m.scroll.map_or("-".to_owned(), |scroll| format!("{}:{}", scroll.direction.as_str(), num(scroll.delta)));
    format!(
        "type={} button={} x={} y={} mods={}{}{} scroll={}",
        m.kind.as_str(),
        m.button,
        num(m.x),
        num(m.y),
        flag(m.modifiers.shift),
        flag(m.modifiers.alt),
        flag(m.modifiers.ctrl),
        scroll
    )
}

fn event(e: &StdinEvent) -> String {
    match e {
        StdinEvent::Key { raw, key: k } => format!("E key raw={} {}", esc(raw), key(k)),
        StdinEvent::Mouse { raw, encoding, event } => {
            format!("E mouse raw={} {} {}", esc(raw), encoding.as_str(), mouse(event))
        }
        StdinEvent::Paste { bytes, metadata } => {
            assert!(metadata.is_none());
            format!("E paste {} meta=-", hex(bytes))
        }
        StdinEvent::Response { protocol, sequence } => format!("E resp {} {}", protocol.as_str(), esc(sequence)),
    }
}

struct ManualClock(Cell<f64>);

impl Clock for ManualClock {
    fn now(&self) -> f64 {
        self.0.get()
    }
}

struct Program {
    clock: Rc<ManualClock>,
    parser: Option<StdinParser>,
    out: Vec<String>,
}

fn set_field(patch: &mut ProtocolContextPatch, field: usize, value: bool) {
    let slot = match field {
        0 => &mut patch.kitty_keyboard_enabled,
        1 => &mut patch.private_capability_replies_active,
        2 => &mut patch.pixel_resolution_query_active,
        3 => &mut patch.explicit_width_cpr_active,
        4 => &mut patch.startup_cursor_cpr_active,
        _ => &mut patch.reply_sentinel_active,
    };
    *slot = Some(value);
}

fn option_number(token: &str) -> Option<f64> {
    (token != "-").then(|| token.parse().unwrap())
}

impl Program {
    fn parser(&mut self) -> &mut StdinParser {
        self.parser.as_mut().unwrap()
    }

    fn drain(&mut self) -> Result<(), Destroyed> {
        let mut events = Vec::new();
        let result = self.parser().drain(|e| events.push(event(&e)));
        self.out.extend(events);
        result
    }

    fn guard(&mut self, result: Result<(), Destroyed>) {
        if let Err(error) = result {
            self.out.push(format!("err {}", esc(&error.to_string())));
        }
    }

    /// The manual clock's `advance`: the armed timeout fires at its deadline, then the renderer drains.
    fn advance(&mut self, ms: f64) {
        let target = self.clock.0.get() + ms;
        while let Some(deadline) = self.parser().timeout_deadline() {
            if deadline > target {
                break;
            }
            self.clock.0.set(deadline);
            let parser = self.parser();
            if parser.is_destroyed() {
                parser.on_timeout();
                continue;
            }
            parser.on_timeout();
            self.out.push(format!("T {}", num(self.clock.0.get())));
            let result = self.drain();
            self.guard(result);
        }
        self.clock.0.set(target);
    }

    fn exec(&mut self, op: &str) {
        let tokens: Vec<&str> = op.split(' ').collect();
        let (name, args) = (tokens[0], &tokens[1..]);
        match name {
            "new" => {
                let mut context = ProtocolContextPatch::default();
                for (field, &bit) in args[4].as_bytes().iter().enumerate() {
                    if bit != b'-' {
                        set_field(&mut context, field, bit == b'1');
                    }
                }
                let parser = StdinParser::new(StdinParserOptions {
                    timeout_ms: option_number(args[0]),
                    max_pending_bytes: option_number(args[1]),
                    arm_timeouts: Some(args[2] == "1"),
                    use_kitty_keyboard: (args[3] != "-").then(|| args[3] == "1"),
                    protocol_context: context,
                    clock: Some(self.clock.clone()),
                });
                self.out.push(format!("cap {}", parser.buffer_capacity()));
                self.parser = Some(parser);
            }
            "push" => {
                let result = self.parser().push(&unhex(args[0])).and_then(|()| self.drain());
                self.guard(result);
            }
            "adv" => {
                self.advance(args[0].parse().unwrap());
                let now = num(self.clock.0.get());
                self.out.push(format!("now {now}"));
            }
            "ctx" => {
                let mut patch = ProtocolContextPatch::default();
                set_field(&mut patch, args[0].parse().unwrap(), args[1] == "1");
                let drain = args[2] == "1";
                let result = self
                    .parser()
                    .update_protocol_context(patch)
                    .and_then(|()| if drain { self.drain() } else { Ok(()) });
                self.guard(result);
            }
            "abort" => {
                let result = self.parser().abort_pending_startup_cursor_cpr();
                self.guard(result);
            }
            "pause" => {
                let result = self.parser().pause_pending_timeout();
                self.guard(result);
            }
            "resume" => {
                let result = self.parser().resume_pending_timeout();
                self.guard(result);
            }
            "mreset" => {
                let result = self.parser().reset_mouse_state();
                self.guard(result);
            }
            "reset" => self.parser().reset(),
            "destroy" => self.parser().destroy(),
            "flush" => {
                let now = (args[0] != "-").then(|| self.clock.0.get() + args[0].parse::<f64>().unwrap());
                let result = self.parser().flush_timeout(now).and_then(|()| self.drain());
                self.guard(result);
            }
            "read" => match self.parser().read() {
                Ok(Some(e)) => self.out.push(event(&e)),
                Ok(None) => self.out.push("none".to_owned()),
                Err(error) => self.out.push(format!("err {}", esc(&error.to_string()))),
            },
            "pix" => {
                let pending = self.parser().has_pending_pixel_resolution_response();
                self.out.push(format!("pix {}", pending as u8));
            }
            "cap" => {
                let capacity = self.parser().buffer_capacity();
                self.out.push(format!("cap {capacity}"));
            }
            "pk" => {
                let text = js::decode_utf8(&unhex(args[0]));
                let parsed = parse_keypress(&text, args[1] == "1");
                self.out.push(parsed.as_ref().map_or("null".to_owned(), key));
            }
            "pkb" => {
                let parsed = keypress::parse_keypress_high_byte(args[0].parse().unwrap(), args[1] == "1");
                self.out.push(parsed.as_ref().map_or("null".to_owned(), key));
            }
            "mall" => {
                let mut parser = MouseParser::new();
                for chunk in args {
                    let events = parser.parse_all_mouse_events(&unhex(chunk));
                    let line = events.iter().map(mouse).collect::<Vec<_>>().join(" ; ");
                    self.out.push(if line.is_empty() { "none".to_owned() } else { line });
                }
            }
            "strip" => {
                let text = js::decode_utf8(&unhex(args[0]));
                self.out.push(esc(&strip_ansi_sequences(&text)));
            }
            "dec" => self.out.push(esc(&decode_paste_bytes(&unhex(args[0])))),
            _ => panic!("op {op}"),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let programs = std::fs::read_to_string(&args[1]).expect("programs");
    let mut results = String::new();
    let mut program: Option<Program> = None;
    for line in programs.lines() {
        if line == "end" {
            if let Some(done) = program.take() {
                for answer in done.out {
                    results.push_str(&answer);
                    results.push('\n');
                }
            }
            results.push_str("end\n");
            continue;
        }
        program
            .get_or_insert_with(|| Program {
                clock: Rc::new(ManualClock(Cell::new(0.0))),
                parser: None,
                out: Vec::new(),
            })
            .exec(line);
    }
    std::fs::write(&args[2], results).expect("results");
}
