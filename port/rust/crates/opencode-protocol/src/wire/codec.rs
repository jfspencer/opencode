//! Wire types for the Effect transformations whose decoding the Rust side reproduces.

use super::number::{is_js_whitespace, js_number_string, js_string_to_number, js_trim};
use super::{Issue, Json, Out, Wire, check, parse};

/// `Schema.Number` under the JSON codec: a finite number, or `"NaN"`, `"Infinity"`, `"-Infinity"`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct JsNumber(pub f64);

impl Wire for JsNumber {
    fn decode(v: &Json) -> Result<Self, Issue> {
        match v {
            Json::Number(n) if n.is_finite() => Ok(Self(*n)),
            Json::String(s) if s == "NaN" => Ok(Self(f64::NAN)),
            Json::String(s) if s == "Infinity" => Ok(Self(f64::INFINITY)),
            Json::String(s) if s == "-Infinity" => Ok(Self(f64::NEG_INFINITY)),
            _ => Err(Issue::expected("a finite number or \"NaN\" | \"Infinity\" | \"-Infinity\"", v)),
        }
    }
    fn encode(&self, out: &mut Out) {
        if self.0.is_finite() { out.number(self.0) } else { out.string(&js_number_string(self.0)) }
    }
}

/// `Schema.NumberFromString`: the string is read with JS `Number(s)` (so `""` is 0 and `"0x10"` is 16) and written
/// back with `String(n)`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NumberString(pub f64);

impl NumberString {
    pub fn decode_with(v: &Json, wire: impl Fn(&str) -> Result<(), Issue>) -> Result<f64, Issue> {
        let s = String::decode(v)?;
        wire(&s)?;
        Ok(js_string_to_number(&s))
    }
}

impl Wire for NumberString {
    fn decode(v: &Json) -> Result<Self, Issue> {
        Self::decode_with(v, |_| Ok(())).map(Self)
    }
    fn encode(&self, out: &mut Out) {
        out.string(&js_number_string(self.0))
    }
}

/// `Schema.NumberFromString` checked as an integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IntString(pub i64);

impl Wire for IntString {
    fn decode(v: &Json) -> Result<Self, Issue> {
        let n = NumberString::decode_with(v, |_| Ok(()))?;
        check::int(n)?;
        Ok(Self(n as i64))
    }
    fn encode(&self, out: &mut Out) {
        out.string(&self.0.to_string())
    }
}

/// `DateTimeUtcFromMillis`: epoch milliseconds. `new Date(n)` truncates to an integer and rejects |n| > 8.64e15
/// (TS fails with a defect there, so the HTTP layer answers 500 rather than 400).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct EpochMillis(pub i64);

impl Wire for EpochMillis {
    fn decode(v: &Json) -> Result<Self, Issue> {
        let n = f64::decode(v)?;
        check::finite(n)?;
        if n.abs() > 8.64e15 {
            return Err(Issue::new("Invalid date"));
        }
        Ok(Self(n.trunc() as i64))
    }
    fn encode(&self, out: &mut Out) {
        out.int(self.0)
    }
}

/// `Schema.Struct({})`: any value except `null`, returned unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct NotNull(pub Json);

impl Default for NotNull {
    fn default() -> Self {
        Self(Json::Object(Default::default()))
    }
}

impl Wire for NotNull {
    fn decode(v: &Json) -> Result<Self, Issue> {
        match v {
            Json::Null => Err(Issue::expected("object | array", v)),
            _ => Ok(Self(v.clone())),
        }
    }
    fn encode(&self, out: &mut Out) {
        out.json(&self.0)
    }
}

/// `Schema.fromJsonString(T)`: a string holding the JSON text of `T`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JsonString<T>(pub T);

impl<T: Wire> Wire for JsonString<T> {
    fn decode(v: &Json) -> Result<Self, Issue> {
        let s = String::decode(v)?;
        let inner = parse(&s).map_err(|_| Issue::new("expected a valid JSON string"))?;
        T::decode(&inner).map(Self)
    }
    fn encode(&self, out: &mut Out) {
        out.string(&super::to_string(&self.0))
    }
}

/// `Schema.Trim`: decoding trims with JS `String.prototype.trim`; `wire` checks the raw text, `checks` the result.
pub fn trim(
    v: &Json,
    wire: impl Fn(&str) -> Result<(), Issue>,
    checks: impl Fn(&str) -> Result<(), Issue>,
) -> Result<String, Issue> {
    let s = String::decode(v)?;
    wire(&s)?;
    let t = js_trim(&s).to_owned();
    checks(&t)?;
    Ok(t)
}

/// `Uint8Array` under the JSON codec: standard base64 with padding (CR/LF ignored on input).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Base64(pub Vec<u8>);

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

impl Wire for Base64 {
    fn decode(v: &Json) -> Result<Self, Issue> {
        let s = String::decode(v)?;
        // effect/encoding/Base64.decode: strip CR/LF; length % 4 == 0; '=' only as the last one or two characters.
        let s: Vec<char> = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        let invalid = || Issue::new("expected a valid Base64 string");
        if s.len() % 4 != 0 {
            return Err(invalid());
        }
        if let Some(i) = s.iter().position(|c| *c == '=')
            && (i + 2 < s.len() || (i + 2 == s.len() && s[s.len() - 1] != '='))
        {
            return Err(invalid());
        }
        let code = |c: char| -> Result<u32, Issue> {
            if c == '=' {
                return Ok(0);
            }
            ALPHABET.iter().position(|a| *a as char == c).map(|p| p as u32).ok_or_else(invalid)
        };
        let missing = if s.ends_with(&['=', '=']) { 2 } else if s.ends_with(&['=']) { 1 } else { 0 };
        let mut bytes = Vec::with_capacity(s.len() / 4 * 3);
        for chunk in s.chunks(4) {
            let b = code(chunk[0])? << 18 | code(chunk[1])? << 12 | code(chunk[2])? << 6 | code(chunk[3])?;
            bytes.extend([(b >> 16) as u8, (b >> 8) as u8, b as u8]);
        }
        bytes.truncate(bytes.len() - missing);
        Ok(Self(bytes))
    }
    fn encode(&self, out: &mut Out) {
        let mut s = String::with_capacity(self.0.len().div_ceil(3) * 4);
        for chunk in self.0.chunks(3) {
            let b = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    s.push(ALPHABET[(b >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        out.string(&s)
    }
}

/// `Schema.DurationFromString`: Effect duration text (`"5 minutes"`, `"1.5 seconds"`, `"Infinity"`), written back
/// as `String(duration)`: `"<n> millis"`, `"<n> nanos"`, `"Infinity"` or `"-Infinity"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Duration {
    Millis(f64),
    Nanos(i128),
    Infinity,
    NegativeInfinity,
}

impl Duration {
    /// Effect's `Duration.fromInput` for strings, followed by `make`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Infinity" => return Some(Self::Infinity),
            "-Infinity" => return Some(Self::NegativeInfinity),
            _ => {}
        }
        // /^(-?\d+(?:\.\d+)?)\s+(nanos?|micros?|millis?|seconds?|minutes?|hours?|days?|weeks?)$/
        let value_end = {
            let b = s.as_bytes();
            let mut i = usize::from(b.first() == Some(&b'-'));
            let int_start = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == int_start {
                return None;
            }
            if b.get(i) == Some(&b'.') {
                let frac = i + 1;
                let mut j = frac;
                while b.get(j).is_some_and(u8::is_ascii_digit) {
                    j += 1;
                }
                if j > frac {
                    i = j;
                }
            }
            i
        };
        let value = &s[..value_end];
        let rest = &s[value_end..];
        let unit = rest.trim_start_matches(is_js_whitespace);
        if unit.len() == rest.len() {
            return None;
        }
        let nanos = |scale: i128| Some(Self::make_nanos(parse_nanos(value, scale)?));
        match unit {
            "nano" | "nanos" => nanos(1),
            "micro" | "micros" => nanos(1_000),
            "milli" | "millis" => Self::from_millis_string(value, 1),
            "second" | "seconds" => Self::from_millis_string(value, 1_000),
            "minute" | "minutes" => Self::from_millis_string(value, 60_000),
            "hour" | "hours" => Self::from_millis_string(value, 3_600_000),
            "day" | "days" => Self::from_millis_string(value, 86_400_000),
            "week" | "weeks" => Self::from_millis_string(value, 604_800_000),
            _ => None,
        }
    }

    fn from_millis_string(value: &str, scale: i64) -> Option<Self> {
        let millis = js_string_to_number(value) * scale as f64;
        if !millis.is_finite() {
            return Some(Self::make_millis(millis));
        }
        let nanos = parse_nanos(value, scale as i128 * 1_000_000)?;
        if millis.trunc() == millis && (millis as i128).checked_mul(1_000_000) == Some(nanos) {
            Some(Self::make_millis(millis))
        } else {
            Some(Self::make_nanos(nanos))
        }
    }

    fn make_millis(n: f64) -> Self {
        if n.is_nan() || n == 0.0 {
            Self::Millis(0.0)
        } else if n.is_infinite() {
            if n > 0.0 { Self::Infinity } else { Self::NegativeInfinity }
        } else if n.trunc() != n {
            // roundMillisToNanos; not reachable from strings, kept for completeness.
            Self::Nanos((n * 1e6).round() as i128)
        } else {
            Self::Millis(n)
        }
    }

    fn make_nanos(n: i128) -> Self {
        if n == 0 { Self::Millis(0.0) } else { Self::Nanos(n) }
    }
}

// parseNanos: the decimal value times `scale`, rounded half away from zero. Values beyond i128 are rejected
// (TS uses BigInt and accepts them).
fn parse_nanos(input: &str, scale: i128) -> Option<i128> {
    let negative = input.starts_with('-');
    let digits = input.trim_start_matches('-');
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let int: i128 = int.parse().ok()?;
    if frac.is_empty() {
        let v = int.checked_mul(scale)?;
        return Some(if negative { -v } else { v });
    }
    let frac_scale = 10i128.checked_pow(frac.len() as u32)?;
    let scaled = int.checked_mul(frac_scale)?.checked_add(frac.parse::<i128>().ok()?)?.checked_mul(scale)?;
    let rounded = scaled / frac_scale + i128::from((scaled % frac_scale) * 2 >= frac_scale);
    Some(if negative { -rounded } else { rounded })
}

impl std::fmt::Display for Duration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Infinity => f.write_str("Infinity"),
            Self::NegativeInfinity => f.write_str("-Infinity"),
            Self::Nanos(n) => write!(f, "{n} nanos"),
            Self::Millis(n) => write!(f, "{} millis", js_number_string(*n)),
        }
    }
}

impl Wire for Duration {
    fn decode(v: &Json) -> Result<Self, Issue> {
        Self::parse(&String::decode(v)?).ok_or_else(|| Issue::new("expected a valid Duration string"))
    }
    fn encode(&self, out: &mut Out) {
        out.string(&self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_normalize_like_effect() {
        for (input, output) in [
            ("5 minutes", "300000 millis"),
            ("1.5 seconds", "1500 millis"),
            ("0.0000001 seconds", "100 nanos"),
            ("2 nanos", "2 nanos"),
            ("0 nanos", "0 millis"),
            ("-0 millis", "0 millis"),
            ("Infinity", "Infinity"),
        ] {
            assert_eq!(Duration::parse(input).map(|d| d.to_string()).as_deref(), Some(output), "{input}");
        }
        for input in ["5", "5minutes", "1e3 millis", "5 fortnights", "+5 millis", "5. millis"] {
            assert!(Duration::parse(input).is_none(), "{input}");
        }
    }
}
