//! Exact JavaScript number <-> string conversions, matching JavaScriptCore as run by bun.
//!
//! - `to_string` is the shortest round-trip form (ryu-js), with the ECMAScript exponent rules.
//! - `to_fixed`, `to_exponential` and `to_precision` round the exact decimal value of the double half
//!   up (the spec's "pick the larger n"), from the shortest digits when that is provably the same and
//!   from an exact big-integer expansion otherwise.
//! - `to_string_radix` reproduces JSC's `toStringWithRadix`: the integer part exactly, the fraction
//!   digit by digit with `Uint16WithFraction` arithmetic until the remainder is within half the gap to
//!   the neighbouring doubles.
//! - `to_locale_string_en_us` reproduces ICU's default en-US formatting: the shortest digits rounded
//!   half up ("halfExpand") to three fraction digits, grouped by thousands.
//! - The parsers follow JSC's `jsToNumber`, `parseInt` and `parseFloat`, including their precision for
//!   long digit strings (correct rounding for decimal, JSC's digit-by-digit sums otherwise).

const DIGIT_CHARS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// 2^53: JSC's `mantissaOverflowLowerBound`.
const MANTISSA_OVERFLOW_LOWER_BOUND: f64 = 9007199254740992.0;

// ---------------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------------

/// Writes the decimal digits of `v` at the end of `buf`, returning the index of the first digit.
fn u64_digits(mut v: u64, buf: &mut [u8; 20]) -> usize {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            return i;
        }
    }
}

fn u128_digits(mut v: u128, buf: &mut [u8; 40]) -> usize {
    let mut i = buf.len();
    while v > u64::MAX as u128 {
        let mut chunk = (v % 10_000_000_000_000_000_000) as u64;
        v /= 10_000_000_000_000_000_000;
        for _ in 0..19 {
            i -= 1;
            buf[i] = b'0' + (chunk % 10) as u8;
            chunk /= 10;
        }
    }
    let mut w = v as u64;
    loop {
        i -= 1;
        buf[i] = b'0' + (w % 10) as u8;
        w /= 10;
        if w == 0 {
            return i;
        }
    }
}

fn push_u64(out: &mut String, v: u64) {
    let mut buf = [0u8; 20];
    let start = u64_digits(v, &mut buf);
    out.push_str(std::str::from_utf8(&buf[start..]).unwrap());
}

fn push_zeros(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push('0');
    }
}

fn push_ascii(out: &mut String, bytes: &[u8]) {
    out.push_str(std::str::from_utf8(bytes).unwrap());
}

/// `x = m * 2^e` for a positive finite `x`, with `m` odd (or `x == 0`, which callers exclude).
fn decompose(x: f64) -> (u64, i32) {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & 0x000f_ffff_ffff_ffff;
    let (mut m, mut e) = if exp == 0 { (frac, -1074) } else { (frac | (1u64 << 52), exp - 1075) };
    if m != 0 {
        let tz = m.trailing_zeros();
        m >>= tz;
        e += tz as i32;
    }
    (m, e)
}

/// String(i) for integral `x` with |x| < 2^64.
fn integer_to_string(x: f64) -> String {
    let mut out = String::with_capacity(21);
    if x < 0.0 {
        out.push('-');
    }
    push_u64(&mut out, x.abs() as u64);
    out
}

// ---------------------------------------------------------------------------------------------------
// Big integers (just enough for exact decimal and radix expansions of doubles)
// ---------------------------------------------------------------------------------------------------

/// Little-endian base-2^32 natural number. 84 limbs hold m * 5^1074 (< 2^2547) and m * 2^971.
struct Big {
    limbs: [u32; 84],
    len: usize,
}

impl Big {
    fn from_u64(v: u64) -> Big {
        let mut b = Big { limbs: [0; 84], len: 0 };
        b.limbs[0] = v as u32;
        b.limbs[1] = (v >> 32) as u32;
        b.len = if b.limbs[1] != 0 {
            2
        } else if b.limbs[0] != 0 {
            1
        } else {
            0
        };
        b
    }

    fn is_zero(&self) -> bool {
        self.len == 0
    }

    fn mul_small(&mut self, m: u32) {
        let mut carry: u64 = 0;
        for limb in &mut self.limbs[..self.len] {
            let v = *limb as u64 * m as u64 + carry;
            *limb = v as u32;
            carry = v >> 32;
        }
        if carry != 0 {
            self.limbs[self.len] = carry as u32;
            self.len += 1;
        }
    }

    fn mul_pow5(&mut self, mut k: u32) {
        const P13: u32 = 1_220_703_125; // 5^13
        while k >= 13 {
            self.mul_small(P13);
            k -= 13;
        }
        if k > 0 {
            self.mul_small(5u32.pow(k));
        }
    }

    /// Divides in place, returning the remainder.
    fn divmod_small(&mut self, d: u32) -> u32 {
        let mut rem: u64 = 0;
        for i in (0..self.len).rev() {
            let cur = (rem << 32) | self.limbs[i] as u64;
            self.limbs[i] = (cur / d as u64) as u32;
            rem = cur % d as u64;
        }
        while self.len > 0 && self.limbs[self.len - 1] == 0 {
            self.len -= 1;
        }
        rem as u32
    }
}

/// `m << e` as a big integer.
fn big_shifted(m: u64, e: u32) -> Big {
    let mut b = Big { limbs: [0; 84], len: 0 };
    let words = (e / 32) as usize;
    let rem = e % 32;
    let wide = (m as u128) << rem;
    b.limbs[words] = wide as u32;
    b.limbs[words + 1] = (wide >> 32) as u32;
    b.limbs[words + 2] = (wide >> 64) as u32;
    b.len = words + 3;
    while b.len > 0 && b.limbs[b.len - 1] == 0 {
        b.len -= 1;
    }
    b
}

/// Writes the decimal digits of `b` (consumed) into `out`, returning the digit count.
fn big_to_decimal(b: &mut Big, out: &mut [u8]) -> usize {
    let mut chunks = [0u32; 96];
    let mut n = 0;
    while !b.is_zero() {
        chunks[n] = b.divmod_small(1_000_000_000);
        n += 1;
    }
    if n == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut len = 0;
    let mut buf = [0u8; 20];
    let start = u64_digits(chunks[n - 1] as u64, &mut buf);
    for &c in &buf[start..] {
        out[len] = c;
        len += 1;
    }
    for i in (0..n - 1).rev() {
        let mut c = chunks[i];
        for k in (0..9).rev() {
            out[len + k] = b'0' + (c % 10) as u8;
            c /= 10;
        }
        len += 9;
    }
    len
}

// ---------------------------------------------------------------------------------------------------
// Decimal digit strings: value = 0.d[0]d[1]...d[len-1] * 10^point, digits ASCII, no trailing zeros
// ---------------------------------------------------------------------------------------------------

/// Shortest round-trip digits of a positive finite double (ryu-js).
struct Shortest {
    d: [u8; 24],
    len: usize,
    point: i32,
}

fn shortest(x: f64) -> Shortest {
    let mut buffer = ryu_js::Buffer::new();
    let s = buffer.format_finite(x).as_bytes();
    let mut r = Shortest { d: [0; 24], len: 0, point: 0 };
    let mut seen_dot = false;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b'0'..=b'9' => {
                if r.len == 0 && c == b'0' {
                    if seen_dot {
                        r.point -= 1;
                    }
                } else {
                    r.d[r.len] = c;
                    r.len += 1;
                    if !seen_dot {
                        r.point += 1;
                    }
                }
            }
            b'.' => seen_dot = true,
            b'e' | b'E' => {
                i += 1;
                let mut neg = false;
                if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
                    neg = s[i] == b'-';
                    i += 1;
                }
                let mut e = 0i32;
                while i < s.len() {
                    e = e * 10 + (s[i] - b'0') as i32;
                    i += 1;
                }
                r.point += if neg { -e } else { e };
                break;
            }
            _ => {}
        }
        i += 1;
    }
    while r.len > 0 && r.d[r.len - 1] == b'0' {
        r.len -= 1;
    }
    r
}

/// The exact decimal expansion of a positive finite double.
struct Exact {
    d: [u8; 800],
    len: usize,
    point: i32,
}

fn exact_decimal(x: f64) -> Exact {
    let (m, e) = decompose(x);
    let mut r = Exact { d: [0; 800], len: 0, point: 0 };
    if e >= 0 {
        if e + (64 - m.leading_zeros() as i32) <= 128 {
            let mut buf = [0u8; 40];
            let start = u128_digits((m as u128) << e, &mut buf);
            r.len = 40 - start;
            r.d[..r.len].copy_from_slice(&buf[start..]);
        } else {
            let mut b = big_shifted(m, e as u32);
            r.len = big_to_decimal(&mut b, &mut r.d);
        }
        r.point = r.len as i32;
    } else {
        let k = (-e) as u32;
        if k <= 27 {
            let mut buf = [0u8; 40];
            let start = u128_digits(m as u128 * 5u128.pow(k), &mut buf);
            r.len = 40 - start;
            r.d[..r.len].copy_from_slice(&buf[start..]);
        } else {
            let mut b = Big::from_u64(m);
            b.mul_pow5(k);
            r.len = big_to_decimal(&mut b, &mut r.d);
        }
        r.point = r.len as i32 - k as i32;
    }
    while r.len > 0 && r.d[r.len - 1] == b'0' {
        r.len -= 1;
    }
    r
}

/// Rounds `d[..len]` half up to `keep` digits (which may be <= 0), updating `point`. Returns the new
/// length with trailing zeros removed (0 means the value rounded to zero).
fn round_half_up(d: &mut [u8], len: usize, point: &mut i32, keep: i64) -> usize {
    if keep >= len as i64 {
        return len;
    }
    if keep < 0 {
        return 0;
    }
    let keep = keep as usize;
    if d[keep] < b'5' {
        let mut n = keep;
        while n > 0 && d[n - 1] == b'0' {
            n -= 1;
        }
        return n;
    }
    // Round up: drop trailing nines, then increment.
    let mut n = keep;
    while n > 0 && d[n - 1] == b'9' {
        n -= 1;
    }
    if n == 0 {
        d[0] = b'1';
        *point += 1;
        return 1;
    }
    d[n - 1] += 1;
    n
}

/// Rounds a positive finite double half up to `p` significant digits (1..=101): digits (without
/// trailing zeros) and the decimal point position.
fn round_significant(a: f64, p: usize, out: &mut [u8; 112]) -> (usize, i32) {
    let s = shortest(a);
    if s.len <= p {
        // The shortest digits are the exact rounding when the p-digit grid is coarser than the
        // double's precision (p <= 15 for a normal double).
        if p <= 15 && a >= f64::MIN_POSITIVE {
            out[..s.len].copy_from_slice(&s.d[..s.len]);
            return (s.len, s.point);
        }
    } else if !(s.len == p + 1 && s.d[p] == b'5') {
        // No rounding midpoint can lie between the double and its shortest representation (or the
        // shortest digits would not be shortest), so rounding them rounds the double.
        let mut d = s.d;
        let mut point = s.point;
        let n = round_half_up(&mut d, s.len, &mut point, p as i64);
        out[..n].copy_from_slice(&d[..n]);
        return (n, point);
    }
    let mut e = exact_decimal(a);
    let mut point = e.point;
    let n = round_half_up(&mut e.d, e.len, &mut point, p as i64);
    out[..n].copy_from_slice(&e.d[..n]);
    (n, point)
}

/// Appends `d` (value 0.d * 10^point) in fixed notation with exactly `f` fraction digits.
fn push_fixed(out: &mut String, d: &[u8], point: i32, f: usize) {
    if point <= 0 {
        out.push('0');
    } else {
        let p = point as usize;
        if d.len() >= p {
            push_ascii(out, &d[..p]);
        } else {
            push_ascii(out, d);
            push_zeros(out, p - d.len());
        }
    }
    if f > 0 {
        out.push('.');
        for j in 1..=f as i64 {
            let k = point as i64 - 1 + j;
            if k >= 0 && (k as usize) < d.len() {
                out.push(d[k as usize] as char);
            } else {
                out.push('0');
            }
        }
    }
}

fn push_exponent(out: &mut String, e: i32) {
    out.push('e');
    out.push(if e < 0 { '-' } else { '+' });
    push_u64(out, e.unsigned_abs() as u64);
}

// ---------------------------------------------------------------------------------------------------
// Number -> string
// ---------------------------------------------------------------------------------------------------

/// Number::toString(x) in radix 10.
pub fn to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".to_string() } else { "-Infinity".to_string() };
    }
    if x == 0.0 {
        return "0".to_string();
    }
    if x.abs() < MANTISSA_OVERFLOW_LOWER_BOUND && x.trunc() == x {
        return integer_to_string(x);
    }
    let mut buffer = ryu_js::Buffer::new();
    buffer.format_finite(x).to_string()
}

/// String(i) for a non-negative integer index.
pub fn index_to_string(i: u64) -> String {
    let mut out = String::with_capacity(20);
    push_u64(&mut out, i);
    out
}

/// Number.prototype.toFixed(digits), digits 0..=100.
pub fn to_fixed(x: f64, digits: u32) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.abs() >= 1e21 {
        return to_string(x);
    }
    let f = digits as usize;
    let mut out = String::with_capacity(24 + f);
    if x < 0.0 {
        out.push('-');
    }
    let a = x.abs();
    if a == 0.0 {
        out.push('0');
        if f > 0 {
            out.push('.');
            push_zeros(&mut out, f);
        }
        return out;
    }
    let (m, e) = decompose(a);
    if e >= 0 {
        // An integer below 1e21 < 2^70.
        let mut buf = [0u8; 40];
        let start = u128_digits((m as u128) << e, &mut buf);
        push_ascii(&mut out, &buf[start..]);
        if f > 0 {
            out.push('.');
            push_zeros(&mut out, f);
        }
        return out;
    }
    if f <= 22 {
        // round(a * 10^f) = (m * 10^f + 2^(s-1)) >> s, all below 2^128.
        let s = (-e) as u32;
        let n = m as u128 * 10u128.pow(f as u32);
        let q = if s >= 128 { 0 } else { (n + (1u128 << (s - 1))) >> s };
        let mut buf = [0u8; 40];
        let start = u128_digits(q, &mut buf);
        let ds = &buf[start..];
        if ds.len() <= f {
            out.push('0');
            if f > 0 {
                out.push('.');
                push_zeros(&mut out, f - ds.len());
                push_ascii(&mut out, ds);
            }
        } else {
            push_ascii(&mut out, &ds[..ds.len() - f]);
            if f > 0 {
                out.push('.');
                push_ascii(&mut out, &ds[ds.len() - f..]);
            }
        }
        return out;
    }
    let mut ex = exact_decimal(a);
    let mut point = ex.point;
    let keep = point as i64 + f as i64;
    let n = round_half_up(&mut ex.d, ex.len, &mut point, keep);
    push_fixed(&mut out, &ex.d[..n], point, f);
    out
}

/// Number.prototype.toExponential(digits): None for an undefined argument, else 0..=100.
pub fn to_exponential(x: f64, digits: Option<u32>) -> String {
    if !x.is_finite() {
        return to_string(x);
    }
    let mut out = String::with_capacity(32);
    if x < 0.0 {
        out.push('-');
    }
    let a = x.abs();
    if a == 0.0 {
        out.push('0');
        if let Some(f) = digits
            && f > 0
        {
            out.push('.');
            push_zeros(&mut out, f as usize);
        }
        out.push_str("e+0");
        return out;
    }
    match digits {
        None => {
            let s = shortest(a);
            out.push(s.d[0] as char);
            if s.len > 1 {
                out.push('.');
                push_ascii(&mut out, &s.d[1..s.len]);
            }
            push_exponent(&mut out, s.point - 1);
        }
        Some(f) => {
            let p = f as usize + 1;
            let mut buf = [0u8; 112];
            let (n, point) = round_significant(a, p, &mut buf);
            out.push(buf[0] as char);
            if p > 1 {
                out.push('.');
                push_ascii(&mut out, &buf[1..n.max(1)]);
                push_zeros(&mut out, p - n.max(1));
            }
            push_exponent(&mut out, point - 1);
        }
    }
    out
}

/// Number.prototype.toPrecision(precision), precision 1..=100.
pub fn to_precision(x: f64, precision: u32) -> String {
    if !x.is_finite() {
        return to_string(x);
    }
    let p = precision as usize;
    let mut out = String::with_capacity(p + 16);
    if x < 0.0 {
        out.push('-');
    }
    let a = x.abs();
    if a == 0.0 {
        out.push('0');
        if p > 1 {
            out.push('.');
            push_zeros(&mut out, p - 1);
        }
        return out;
    }
    let mut buf = [0u8; 112];
    let (n, point) = round_significant(a, p, &mut buf);
    for b in &mut buf[n..p] {
        *b = b'0';
    }
    let d = &buf[..p];
    let e = point - 1;
    if e < -6 || e >= p as i32 {
        out.push(d[0] as char);
        if p > 1 {
            out.push('.');
            push_ascii(&mut out, &d[1..]);
        }
        push_exponent(&mut out, e);
    } else if e >= 0 {
        let ip = point as usize;
        push_ascii(&mut out, &d[..ip]);
        if p > ip {
            out.push('.');
            push_ascii(&mut out, &d[ip..]);
        }
    } else {
        out.push_str("0.");
        push_zeros(&mut out, (-point) as usize);
        push_ascii(&mut out, d);
    }
    out
}

/// (x).toLocaleString("en-US") with default options, as bun (ICU) prints it.
pub fn to_locale_string_en_us(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    let mut out = String::with_capacity(32);
    if x.is_sign_negative() {
        out.push('-');
    }
    if x.is_infinite() {
        out.push('∞');
        return out;
    }
    let a = x.abs();
    if a == 0.0 {
        out.push('0');
        return out;
    }
    let mut s = shortest(a);
    let mut point = s.point;
    let keep = point as i64 + 3;
    let n = round_half_up(&mut s.d, s.len, &mut point, keep);
    let d = &s.d[..n];
    if n == 0 {
        out.push('0');
        return out;
    }
    if point <= 0 {
        out.push('0');
    } else {
        let ip = point as usize;
        for i in 0..ip {
            if i > 0 && (ip - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(d.get(i).map_or('0', |&c| c as char));
        }
    }
    if n as i32 > point {
        out.push('.');
        push_zeros(&mut out, (-point).max(0) as usize);
        push_ascii(&mut out, &d[point.max(0) as usize..]);
    }
    out
}

// ---------------------------------------------------------------------------------------------------
// Number.prototype.toString(radix): JSC's toStringWithRadix
// ---------------------------------------------------------------------------------------------------

/// JSC's `Uint16WithFraction`: a small integer part with an exact binary fraction, as base-2^32 words,
/// `v[0]` the units and `v[i]` the i-th fraction word (words past `len` are zero). Values stay below 2^16
/// here (fractions below 1 times a radix, deltas that stop the loop once they pass 1), so JSC's clamp
/// at 2^16 never applies.
#[derive(Clone)]
struct Uint16WithFraction {
    v: [u32; 40],
    len: usize,
    leading_zeros: usize,
}

/// JSC's `decomposeDouble`: value = mantissa * 2^(exponent - 52).
fn decompose_double(number: f64) -> (i32, u64) {
    let bits = number.to_bits();
    let mut exponent = ((bits >> 52) & 0x7ff) as i32 - 0x3ff;
    let mut mantissa = bits & 0x000f_ffff_ffff_ffff;
    if exponent == -0x3ff {
        exponent = if mantissa != 0 { -0x3fe } else { 0 };
    } else {
        mantissa |= 1u64 << 52;
    }
    (exponent, mantissa)
}

impl Uint16WithFraction {
    fn new(number: f64, divide_by_exponent: i32) -> Uint16WithFraction {
        let mut r = Uint16WithFraction { v: [0; 40], len: 0, leading_zeros: 0 };
        let integer_part = number.floor();
        r.v[0] = integer_part as u32;
        r.len = 1;
        let (mut exponent, mantissa) = decompose_double(number - integer_part);
        exponent -= divide_by_exponent;
        let mut zero_bits = -exponent - 1;
        while zero_bits >= 32 {
            r.v[r.len] = 0;
            r.len += 1;
            zero_bits -= 32;
        }
        let mut values = [(mantissa >> 21) as u32, (mantissa << 11) as u32, 0u32];
        if zero_bits != 0 {
            let z = zero_bits as u32;
            values[2] = values[1] << (32 - z);
            values[1] = (values[1] >> z) | (values[0] << (32 - z));
            values[0] >>= z;
        }
        for v in values {
            r.v[r.len] = v;
            r.len += 1;
        }
        while r.len > 1 && r.v[r.len - 1] == 0 {
            r.len -= 1;
        }
        while r.leading_zeros < r.len && r.v[r.leading_zeros] == 0 {
            r.leading_zeros += 1;
        }
        r
    }

    fn mul(&mut self, multiplier: u32) {
        let mut acc: u64 = 0;
        let mut i = self.len;
        while i > self.leading_zeros {
            i -= 1;
            acc += self.v[i] as u64 * multiplier as u64;
            self.v[i] = acc as u32;
            acc >>= 32;
        }
        if self.leading_zeros != 0 && acc != 0 {
            self.leading_zeros -= 1;
            self.v[self.leading_zeros] = acc as u32;
        }
    }

    /// Exact `self < other` (JSC compares word arrays, which can disagree with the values when a
    /// multiplication leaves trailing zero words; bun compares the values).
    fn lt(&self, other: &Uint16WithFraction) -> bool {
        for i in 0..self.len.max(other.len) {
            if self.v[i] != other.v[i] {
                return self.v[i] < other.v[i];
            }
        }
        false
    }

    fn floor_and_subtract(&mut self) -> u32 {
        let floor = self.v[0];
        if floor != 0 {
            self.v[0] = 0;
            self.leading_zeros = 1;
            while self.leading_zeros < self.len && self.v[self.leading_zeros] == 0 {
                self.leading_zeros += 1;
            }
        }
        floor
    }

    /// The sign of `self - 0.5`, exactly.
    fn compare_point5(&self) -> i32 {
        if self.v[0] != 0 || self.v[1] > 0x8000_0000 {
            return 1;
        }
        if self.v[1] < 0x8000_0000 {
            return -1;
        }
        if self.v[2..self.len.max(2)].iter().any(|&w| w != 0) { 1 } else { 0 }
    }

    /// Exact `self + addend > 1`.
    fn sum_greater_than_one(&self, addend: &Uint16WithFraction) -> bool {
        let len = self.len.max(addend.len);
        let mut carry = 0u64;
        let mut fraction_nonzero = false;
        for i in (1..len).rev() {
            let s = self.v[i] as u64 + addend.v[i] as u64 + carry;
            fraction_nonzero |= s as u32 != 0;
            carry = s >> 32;
        }
        let units = self.v[0] as u64 + addend.v[0] as u64 + carry;
        units > 1 || (units == 1 && fraction_nonzero)
    }
}

/// Appends the digits of a non-negative integral double in `radix`.
fn push_integer_radix(out: &mut String, integer_part: f64, radix: u32) {
    if integer_part < 18446744073709551616.0 {
        let mut v = integer_part as u64;
        let mut buf = [0u8; 64];
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = DIGIT_CHARS[(v % radix as u64) as usize];
            v /= radix as u64;
            if v == 0 {
                break;
            }
        }
        push_ascii(out, &buf[i..]);
        return;
    }
    let (m, e) = decompose(integer_part);
    let mut b = big_shifted(m, e as u32);
    // Divide by the largest power of the radix that fits a u32, emitting that many digits per step.
    let mut chunk_digits = 1;
    let mut chunk = radix as u64;
    while chunk * radix as u64 <= u32::MAX as u64 {
        chunk *= radix as u64;
        chunk_digits += 1;
    }
    let mut buf = [0u8; 1100];
    let mut i = buf.len();
    while !b.is_zero() {
        let mut r = b.divmod_small(chunk as u32);
        for _ in 0..chunk_digits {
            i -= 1;
            buf[i] = DIGIT_CHARS[(r % radix) as usize];
            r /= radix;
        }
    }
    while i < buf.len() - 1 && buf[i] == b'0' {
        i += 1;
    }
    push_ascii(out, &buf[i..]);
}

/// Number.prototype.toString(radix), radix 2..=36.
pub fn to_string_radix(x: f64, radix: u32) -> String {
    if radix == 10 || !x.is_finite() {
        return to_string(x);
    }
    let int = x as i32;
    if int as f64 == x {
        let mut out = String::with_capacity(34);
        if int < 0 {
            out.push('-');
        }
        push_integer_radix(&mut out, (int as i64).unsigned_abs() as f64, radix);
        return out;
    }
    let is_negative = x < 0.0;
    let number = x.abs();
    let mut integer_part = number.floor();
    let fraction_part = number - integer_part;
    let mut frac: Vec<u8> = Vec::new();
    if fraction_part != 0.0 {
        let mut fraction = Uint16WithFraction::new(fraction_part, 0);
        let next = number.next_up();
        let last = number.next_down();
        let mut half_delta_next = Uint16WithFraction::new(next - number, 1);
        let mut half_delta_last = Uint16WithFraction::new(number - last, 1);
        let mut needs_rounding_up = false;
        // Parity of the digits so far read as one integer: an exact half rounds to even.
        let mut odd = (integer_part as u64) & 1 == 1;
        frac.reserve(64);
        loop {
            let c = fraction.compare_point5();
            if c > 0 || (c == 0 && odd) {
                if fraction.sum_greater_than_one(&half_delta_next) {
                    needs_rounding_up = true;
                    break;
                }
            } else if fraction.lt(&half_delta_last) {
                break;
            }
            fraction.mul(radix);
            let digit = fraction.floor_and_subtract();
            frac.push(DIGIT_CHARS[digit as usize]);
            odd = ((odd && radix & 1 == 1) as u32 + digit) & 1 == 1;
            half_delta_next.mul(radix);
            half_delta_last.mul(radix);
        }
        if needs_rounding_up {
            let max_digit = DIGIT_CHARS[radix as usize - 1];
            while frac.last() == Some(&max_digit) {
                frac.pop();
            }
            match frac.last_mut() {
                Some(d) if *d == b'9' => *d = b'a',
                Some(d) => *d += 1,
                None => integer_part += 1.0,
            }
        } else {
            while frac.last() == Some(&b'0') {
                frac.pop();
            }
        }
    }
    let mut out = String::with_capacity(frac.len() + 24);
    if is_negative {
        out.push('-');
    }
    push_integer_radix(&mut out, integer_part, radix);
    if !frac.is_empty() {
        out.push('.');
        push_ascii(&mut out, &frac);
    }
    out
}

// ---------------------------------------------------------------------------------------------------
// String -> number
// ---------------------------------------------------------------------------------------------------

/// JSC's isStrWhiteSpace: WhiteSpace and LineTerminator code points.
fn is_str_white_space(c: u16) -> bool {
    match c {
        0x09..=0x0D | 0x20 => true,
        _ if c < 0xA0 => false,
        0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF => true,
        _ => false,
    }
}

fn is_digit(c: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&c)
}

/// The digit value of `c` in `radix`, if it is one.
fn parse_digit(c: u16, radix: u32) -> Option<u32> {
    let d = match c {
        0x30..=0x39 => (c - 0x30) as u32,
        0x61..=0x7A => (c - 0x61) as u32 + 10,
        0x41..=0x5A => (c - 0x41) as u32 + 10,
        _ => return None,
    };
    if d < radix { Some(d) } else { None }
}

/// JSC's parseIntOverflow: sums digit * radix^k from the least significant digit, in doubles.
fn parse_int_overflow(digits: &[u16], radix: u32) -> f64 {
    let mut number = 0.0f64;
    let mut multiplier = 1.0f64;
    for &c in digits.iter().rev() {
        if multiplier == f64::INFINITY {
            if c != b'0' as u16 {
                number = f64::INFINITY;
                break;
            }
        } else {
            let digit = parse_digit(c, radix).unwrap_or(0);
            number += digit as f64 * multiplier;
        }
        multiplier *= radix as f64;
    }
    number
}

/// Parses ASCII `bytes` (a validated StrDecimalLiteral without Infinity) correctly rounded.
fn parse_validated_decimal(units: &[u16]) -> f64 {
    let mut stack = [0u8; 96];
    let mut heap: Vec<u8>;
    let bytes: &mut [u8] = if units.len() <= stack.len() {
        &mut stack[..units.len()]
    } else {
        heap = vec![0u8; units.len()];
        &mut heap[..]
    };
    for (b, &u) in bytes.iter_mut().zip(units) {
        *b = u as u8;
    }
    // The grammar was validated, so this cannot fail.
    std::str::from_utf8(bytes).ok().and_then(|s| s.parse::<f64>().ok()).unwrap_or(f64::NAN)
}

/// JSC's parseDouble on a prefix: `[+-]? (digits [. digits?] | . digits) ([eE] [+-]? digits)?`.
/// Returns the value and the consumed length, or None when no number starts here.
fn parse_decimal_prefix(s: &[u16]) -> Option<(f64, usize)> {
    let mut i = 0;
    let n = s.len();
    if i < n && (s[i] == b'+' as u16 || s[i] == b'-' as u16) {
        i += 1;
    }
    let int_start = i;
    while i < n && is_digit(s[i]) {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < n && s[i] == b'.' as u16 {
        let mut j = i + 1;
        while j < n && is_digit(s[j]) {
            j += 1;
        }
        frac_digits = j - i - 1;
        if int_digits > 0 || frac_digits > 0 {
            i = j;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return None;
    }
    if i < n && (s[i] == b'e' as u16 || s[i] == b'E' as u16) {
        let mut j = i + 1;
        if j < n && (s[j] == b'+' as u16 || s[j] == b'-' as u16) {
            j += 1;
        }
        let exp_start = j;
        while j < n && is_digit(s[j]) {
            j += 1;
        }
        if j > exp_start {
            i = j;
        }
    }
    // Fast path: a short run of plain digits.
    if frac_digits == 0 && int_digits <= 15 && i == int_start + int_digits {
        let mut v: u64 = 0;
        for &c in &s[int_start..i] {
            v = v * 10 + (c - b'0' as u16) as u64;
        }
        let v = v as f64;
        return Some((if s[0] == b'-' as u16 { -v } else { v }, i));
    }
    Some((parse_validated_decimal(&s[..i]), i))
}

const INFINITY_UNITS: [u16; 8] = [0x49, 0x6E, 0x66, 0x69, 0x6E, 0x69, 0x74, 0x79];

fn starts_with_infinity(s: &[u16]) -> bool {
    s.len() >= 8 && s[..8] == INFINITY_UNITS
}

/// JSC's jsStrDecimalLiteral: a decimal prefix or [+-]Infinity. Returns the value and consumed length.
fn str_decimal_literal(s: &[u16]) -> (f64, usize) {
    if let Some(r) = parse_decimal_prefix(s) {
        return r;
    }
    match s.first() {
        Some(&0x49) if starts_with_infinity(s) => (f64::INFINITY, 8),
        Some(&0x2B) if starts_with_infinity(&s[1..]) => (f64::INFINITY, 9),
        Some(&0x2D) if starts_with_infinity(&s[1..]) => (f64::NEG_INFINITY, 9),
        _ => (f64::NAN, 0),
    }
}

/// A non-decimal integer literal body (at least one digit at `s[0]`): JSC's jsHex/Octal/BinaryIntegerLiteral.
fn integer_literal(s: &[u16], radix: u32) -> (f64, usize) {
    let mut number = 0.0f64;
    let mut i = 0;
    while i < s.len() {
        match parse_digit(s[i], radix) {
            Some(d) => number = number * radix as f64 + d as f64,
            None => break,
        }
        i += 1;
    }
    if number >= MANTISSA_OVERFLOW_LOWER_BOUND {
        number = parse_int_overflow(&s[..i], radix);
    }
    (number, i)
}

/// ToNumber applied to a string (JSC's jsToNumber).
pub fn string_to_number(s: &[u16]) -> f64 {
    if s.len() == 1 {
        let c = s[0];
        if is_digit(c) {
            return (c - b'0' as u16) as f64;
        }
        if is_str_white_space(c) {
            return 0.0;
        }
        return f64::NAN;
    }
    let mut start = 0;
    while start < s.len() && is_str_white_space(s[start]) {
        start += 1;
    }
    if start == s.len() {
        return 0.0;
    }
    let rest = &s[start..];
    let (number, used) = if rest[0] == b'0' as u16 && rest.len() > 2 {
        let c1 = rest[1] | 0x20;
        let radix = match c1 {
            0x78 => 16,
            0x6F => 8,
            0x62 => 2,
            _ => 0,
        };
        if radix != 0 && parse_digit(rest[2], radix).is_some() {
            let (v, n) = integer_literal(&rest[2..], radix);
            (v, n + 2)
        } else {
            str_decimal_literal(rest)
        }
    } else {
        str_decimal_literal(rest)
    };
    if used == 0 {
        return f64::NAN;
    }
    if rest[used..].iter().all(|&c| is_str_white_space(c)) { number } else { f64::NAN }
}

/// The global parseInt(string, radix), `radix` already converted to a number.
pub fn parse_int(s: &[u16], radix: f64) -> f64 {
    let mut radix = to_int32(radix);
    let len = s.len();
    let mut p = 0;
    while p < len && is_str_white_space(s[p]) {
        p += 1;
    }
    let mut sign = 1.0f64;
    if p < len {
        if s[p] == b'+' as u16 {
            p += 1;
        } else if s[p] == b'-' as u16 {
            sign = -1.0;
            p += 1;
        }
    }
    if (radix == 0 || radix == 16)
        && len - p >= 2
        && s[p] == b'0' as u16
        && (s[p + 1] == b'x' as u16 || s[p + 1] == b'X' as u16)
    {
        radix = 16;
        p += 2;
    } else if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let radix = radix as u32;
    let first = p;
    let mut number = 0.0f64;
    while p < len {
        match parse_digit(s[p], radix) {
            Some(d) => number = number * radix as f64 + d as f64,
            None => break,
        }
        p += 1;
    }
    if p == first {
        return f64::NAN;
    }
    if number >= MANTISSA_OVERFLOW_LOWER_BOUND {
        match radix {
            10 => number = parse_validated_decimal(&s[first..p]),
            2 | 4 | 8 | 16 | 32 => number = parse_int_overflow(&s[first..p], radix),
            _ => {}
        }
    }
    sign * number
}

/// The global parseFloat(string).
pub fn parse_float(s: &[u16]) -> f64 {
    if s.len() == 1 {
        let c = s[0];
        return if is_digit(c) { (c - b'0' as u16) as f64 } else { f64::NAN };
    }
    let mut start = 0;
    while start < s.len() && is_str_white_space(s[start]) {
        start += 1;
    }
    if start == s.len() {
        return f64::NAN;
    }
    str_decimal_literal(&s[start..]).0
}

// ---------------------------------------------------------------------------------------------------
// Integer conversions
// ---------------------------------------------------------------------------------------------------

/// ToInt32.
pub fn to_int32(x: f64) -> i32 {
    let i = x as i32;
    if i as f64 == x {
        return i;
    }
    if !x.is_finite() {
        return 0;
    }
    let r = x.trunc() % 4294967296.0;
    (r as i64) as u32 as i32
}

/// ToUint32.
pub fn to_uint32(x: f64) -> u32 {
    to_int32(x) as u32
}
