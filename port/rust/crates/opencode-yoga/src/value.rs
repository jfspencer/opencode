//! `ValueInput` and `parseYogaValue` (`packages/core/src/yoga.internal.ts`): the values the TS setters take
//! (`number | "auto" | `${number}%` | Value | undefined`) and their translation to a unit and a value.
//!
//! A TS number is an f64 that the FFI call narrows to f32; `ValueInput` keeps the f64 and narrows at the same point,
//! with the same rounding (round to nearest, overflow to infinity). A string is kept as it is and parsed by the
//! setter, after its freed-node check, as in TS (a freed node ignores even an invalid string).
use crate::{Error, Unit, Value};

/// What a TS Yoga setter accepts.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueInput {
    /// `undefined`.
    Undefined,
    /// `"auto"`.
    Auto,
    /// A number: points.
    Point(f64),
    /// `"<number>%"`.
    Percent(f64),
    /// A `{ unit, value }` object, passed through unchanged.
    Value(Value),
    /// A string, parsed by [`ValueInput::parse`] when the setter applies it.
    Text(String),
}

impl ValueInput {
    /// `parseYogaValue` of a string: `"auto"`, or a percentage `Number.parseFloat` reads (`"50%"`, `" 1e1%"`,
    /// `"12px%"` is 12). Anything else is the TS error.
    pub fn parse(text: &str) -> Result<ValueInput, Error> {
        if text == "auto" {
            return Ok(ValueInput::Auto);
        }
        if !text.ends_with('%') {
            return Err(Error::InvalidValue(format!("Invalid Yoga value: {text}")));
        }
        let number = parse_float(text);
        if number.is_nan() {
            return Err(Error::InvalidValue(format!("Invalid Yoga percentage value: {text}")));
        }
        Ok(ValueInput::Percent(number))
    }

    /// The unit and value `parseYogaValue` returns, the value narrowed as the FFI call narrows it.
    pub fn resolve(self) -> Result<(Unit, f32), Error> {
        Ok(match self {
            ValueInput::Undefined => (Unit::Undefined, f32::NAN),
            ValueInput::Auto => (Unit::Auto, f32::NAN),
            ValueInput::Point(value) => (Unit::Point, value as f32),
            ValueInput::Percent(value) => (Unit::Percent, value as f32),
            ValueInput::Value(value) => (value.unit, value.value),
            ValueInput::Text(text) => return ValueInput::parse(&text)?.resolve(),
        })
    }
}

impl From<&str> for ValueInput {
    fn from(text: &str) -> Self {
        ValueInput::Text(text.to_owned())
    }
}

impl From<String> for ValueInput {
    fn from(text: String) -> Self {
        ValueInput::Text(text)
    }
}

impl From<f64> for ValueInput {
    fn from(value: f64) -> Self {
        ValueInput::Point(value)
    }
}

impl From<f32> for ValueInput {
    fn from(value: f32) -> Self {
        ValueInput::Point(value as f64)
    }
}

impl From<i32> for ValueInput {
    fn from(value: i32) -> Self {
        ValueInput::Point(value as f64)
    }
}

impl From<u32> for ValueInput {
    fn from(value: u32) -> Self {
        ValueInput::Point(value as f64)
    }
}

impl From<Value> for ValueInput {
    fn from(value: Value) -> Self {
        ValueInput::Value(value)
    }
}

impl<T: Into<ValueInput>> From<Option<T>> for ValueInput {
    fn from(value: Option<T>) -> Self {
        value.map_or(ValueInput::Undefined, Into::into)
    }
}

/// ECMAScript `StrWhiteSpaceChar`: WhiteSpace and LineTerminator.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `Number.parseFloat`: the longest `StrDecimalLiteral` prefix after leading white space, NaN if there is none.
pub(crate) fn parse_float(text: &str) -> f64 {
    let text = text.trim_start_matches(js_space);
    let bytes = text.as_bytes();
    let mut index = 0;
    let negative = match bytes.first() {
        Some(b'-') => {
            index = 1;
            true
        }
        Some(b'+') => {
            index = 1;
            false
        }
        _ => false,
    };
    let sign = if negative { -1.0 } else { 1.0 };
    if text[index..].starts_with("Infinity") {
        return sign * f64::INFINITY;
    }
    let digits = |from: usize| bytes[from..].iter().take_while(|b| b.is_ascii_digit()).count();
    let start = index;
    let integer = digits(index);
    index += integer;
    let mut fraction = 0;
    if bytes.get(index) == Some(&b'.') {
        fraction = digits(index + 1);
        if integer > 0 || fraction > 0 {
            index += 1 + fraction;
        }
    }
    if integer == 0 && fraction == 0 {
        return f64::NAN;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        let mut exponent = index + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        let count = digits(exponent);
        if count > 0 {
            index = exponent + count;
        }
    }
    // The prefix is digits, at most one '.', and an exponent: Rust's parser reads it correctly rounded.
    let magnitude: f64 = text[start..index].parse().unwrap_or(f64::NAN);
    sign * magnitude
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_float_matches_js() {
        let cases: [(&str, f64); 16] = [
            ("50%", 50.0),
            ("  1e1%", 10.0),
            ("12px%", 12.0),
            (".5%", 0.5),
            ("5.%", 5.0),
            ("-0%", -0.0),
            ("+3.25e-1%", 0.325),
            ("1e%", 1.0),
            ("1e+%", 1.0),
            ("Infinity%", f64::INFINITY),
            ("-Infinity%", f64::NEG_INFINITY),
            ("\u{FEFF}\u{3000}7%", 7.0),
            ("0x10%", 0.0),
            ("1_000%", 1.0),
            ("00012.50%", 12.5),
            ("1e400%", f64::INFINITY),
        ];
        for (text, expected) in cases {
            let got = parse_float(text);
            assert_eq!(got.to_bits(), expected.to_bits(), "{text}");
        }
        for text in ["%", ".%", "-%", "e5%", "abc%", "+.e1%", "inf%", "NaN%"] {
            assert!(parse_float(text).is_nan(), "{text}");
        }
    }
}
