//! `packages/core/src/lib/parse.mouse.ts`: SGR (`ESC [ < b ; x ; y M/m`) and X10 (`ESC [ M b x y`) mouse
//! reports. The parser remembers which buttons are down, so SGR motion with a button held is a drag.
//!
//! Coordinates are JS numbers: an SGR report accumulates its digits in an f64, as TS does.
use crate::js::to_int32;

/// `MouseEventType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseEventType {
    Down,
    Up,
    Move,
    Drag,
    DragEnd,
    Drop,
    Over,
    Out,
    Scroll,
}

impl MouseEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            MouseEventType::Down => "down",
            MouseEventType::Up => "up",
            MouseEventType::Move => "move",
            MouseEventType::Drag => "drag",
            MouseEventType::DragEnd => "drag-end",
            MouseEventType::Drop => "drop",
            MouseEventType::Over => "over",
            MouseEventType::Out => "out",
            MouseEventType::Scroll => "scroll",
        }
    }
}

/// `ScrollInfo.direction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl ScrollDirection {
    fn from_button(button: i32) -> ScrollDirection {
        match button {
            0 => ScrollDirection::Up,
            1 => ScrollDirection::Down,
            2 => ScrollDirection::Left,
            _ => ScrollDirection::Right,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ScrollDirection::Up => "up",
            ScrollDirection::Down => "down",
            ScrollDirection::Left => "left",
            ScrollDirection::Right => "right",
        }
    }
}

/// `ScrollInfo`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollInfo {
    pub direction: ScrollDirection,
    pub delta: f64,
}

/// `RawMouseEvent.modifiers`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// `RawMouseEvent`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawMouseEvent {
    pub kind: MouseEventType,
    pub button: i32,
    pub x: f64,
    pub y: f64,
    pub modifiers: MouseModifiers,
    pub scroll: Option<ScrollInfo>,
}

/// `MouseParser`.
#[derive(Clone, Debug, Default)]
pub struct MouseParser {
    /// `mouseButtonsPressed`, one bit per button 0 to 2.
    pressed: u8,
}

fn modifiers(code: i32) -> MouseModifiers {
    MouseModifiers { shift: code & 4 != 0, alt: code & 8 != 0, ctrl: code & 16 != 0 }
}

impl MouseParser {
    pub fn new() -> MouseParser {
        MouseParser::default()
    }

    pub fn reset(&mut self) {
        self.pressed = 0;
    }

    /// `parseMouseEvent(data)`: the report at the start of `data`.
    pub fn parse_mouse_event(&mut self, data: &[u8]) -> Option<RawMouseEvent> {
        self.parse_at(data, 0).map(|(event, _)| event)
    }

    /// `parseAllMouseEvents(data)`: the reports one after another, up to the first that is not one.
    pub fn parse_all_mouse_events(&mut self, data: &[u8]) -> Vec<RawMouseEvent> {
        let mut events = Vec::new();
        let mut offset = 0;
        while offset < data.len() {
            let Some((event, consumed)) = self.parse_at(data, offset) else { break };
            events.push(event);
            offset += consumed;
        }
        events
    }

    /// `parseMouseSequenceAt` over latin1 text: one character per byte.
    fn parse_at(&mut self, data: &[u8], offset: usize) -> Option<(RawMouseEvent, usize)> {
        if !data[offset..].starts_with(b"\x1b[") {
            return None;
        }
        match data.get(offset + 2) {
            Some(b'<') => self.parse_sgr(data, offset),
            Some(b'M') => {
                if offset + 6 > data.len() {
                    return None;
                }
                let button = data[offset + 3] as i32 - 32;
                let x = data[offset + 4] as f64 - 33.0;
                let y = data[offset + 5] as f64 - 33.0;
                Some((Self::decode_basic(button, x, y), 6))
            }
            _ => None,
        }
    }

    fn parse_sgr(&mut self, data: &[u8], offset: usize) -> Option<(RawMouseEvent, usize)> {
        let mut index = offset + 3;
        let mut values = [0f64; 3];
        let mut part = 0;
        let mut has_digit = false;
        while index < data.len() {
            let byte = data[index];
            if byte.is_ascii_digit() {
                has_digit = true;
                values[part] = values[part] * 10.0 + (byte - b'0') as f64;
                index += 1;
                continue;
            }
            match byte {
                b';' => {
                    if !has_digit || part >= 2 {
                        return None;
                    }
                    part += 1;
                    has_digit = false;
                    index += 1;
                }
                b'M' | b'm' => {
                    if !has_digit || part != 2 {
                        return None;
                    }
                    let event = self.decode_sgr(values[0], values[1], values[2], byte == b'M');
                    return Some((event, index - offset + 1));
                }
                _ => return None,
            }
        }
        None
    }

    fn decode_sgr(&mut self, raw_button: f64, wire_x: f64, wire_y: f64, press: bool) -> RawMouseEvent {
        let code = to_int32(raw_button);
        let button = code & 3;
        let is_scroll = code & 64 != 0;
        let is_motion = code & 32 != 0;
        let mut scroll = None;
        let kind = if is_motion {
            if button == 3 || self.pressed == 0 { MouseEventType::Move } else { MouseEventType::Drag }
        } else if is_scroll && press {
            scroll = Some(ScrollInfo { direction: ScrollDirection::from_button(button), delta: 1.0 });
            MouseEventType::Scroll
        } else if press {
            if button != 3 {
                self.pressed |= 1 << button;
            }
            MouseEventType::Down
        } else {
            if button != 3 {
                self.pressed &= !(1 << button);
            }
            MouseEventType::Up
        };
        RawMouseEvent {
            kind,
            button: if button == 3 { 0 } else { button },
            x: wire_x - 1.0,
            y: wire_y - 1.0,
            modifiers: modifiers(code),
            scroll,
        }
    }

    fn decode_basic(code: i32, x: f64, y: f64) -> RawMouseEvent {
        let button = code & 3;
        let (kind, button, scroll) = if code & 32 != 0 {
            (MouseEventType::Move, if button == 3 { -1 } else { button }, None)
        } else if code & 64 != 0 {
            (
                MouseEventType::Scroll,
                0,
                Some(ScrollInfo { direction: ScrollDirection::from_button(button), delta: 1.0 }),
            )
        } else if button == 3 {
            (MouseEventType::Up, 0, None)
        } else {
            (MouseEventType::Down, button, None)
        };
        RawMouseEvent { kind, button, x, y, modifiers: modifiers(code), scroll }
    }
}
