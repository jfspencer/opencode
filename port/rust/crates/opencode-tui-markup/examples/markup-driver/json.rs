//! A JSON writer for the driver's answers: strings are UTF-16 so lone surrogates come out as `\uXXXX` escapes,
//! exactly as JSON.stringify writes them.

use opencode_tui_markup::js::JsString;

pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(JsString),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
    /// An object with computed keys.
    Map(Vec<(JsString, J)>),
}

impl J {
    pub fn s(text: &str) -> J {
        J::Str(JsString::from(text))
    }

    pub fn js(text: &JsString) -> J {
        J::Str(text.clone())
    }

    pub fn opt<T>(value: Option<T>, f: impl FnOnce(T) -> J) -> J {
        value.map_or(J::Null, f)
    }

    pub fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => {
                if n.is_finite() {
                    if n.fract() == 0.0 && n.abs() < 1e15 {
                        out.push_str(&format!("{}", *n as i64));
                    } else {
                        out.push_str(&format!("{n:?}"));
                    }
                } else {
                    out.push_str("null");
                }
            }
            J::Str(s) => write_string(out, s),
            J::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            J::Obj(fields) => {
                out.push('{');
                let mut first = true;
                for (k, v) in fields {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    write_string(out, &JsString::from(*k));
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
            J::Map(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_string(out: &mut String, s: &JsString) {
    out.push('"');
    let units = s.as_slice();
    let mut i = 0;
    while i < units.len() {
        let c = units[i];
        let is_high = (0xd800..0xdc00).contains(&c);
        let is_low = (0xdc00..0xe000).contains(&c);
        if is_high && i + 1 < units.len() && (0xdc00..0xe000).contains(&units[i + 1]) {
            let cp = 0x10000 + (((c as u32) - 0xd800) << 10) + ((units[i + 1] as u32) - 0xdc00);
            out.push(char::from_u32(cp).unwrap());
            i += 2;
            continue;
        }
        if is_high || is_low {
            out.push_str(&format!("\\u{c:04x}"));
        } else {
            match c {
                0x22 => out.push_str("\\\""),
                0x5c => out.push_str("\\\\"),
                0x0a => out.push_str("\\n"),
                0x0d => out.push_str("\\r"),
                0x09 => out.push_str("\\t"),
                0x08 => out.push_str("\\b"),
                0x0c => out.push_str("\\f"),
                c if c < 0x20 => out.push_str(&format!("\\u{c:04x}")),
                c => out.push(char::from_u32(c as u32).unwrap()),
            }
        }
        i += 1;
    }
    out.push('"');
}
